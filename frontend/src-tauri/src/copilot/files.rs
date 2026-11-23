// copilot/files.rs (fork addition)
//
// Read-only access to the folders the user allowed the copilot to use: text extraction
// (text and code, PDF, Word, PowerPoint, Excel) and a keyword search. Every path is
// resolved (symlinks, `..`) and checked to be inside an allowed folder. Nothing here writes
// user files.

use regex::Regex;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use walkdir::WalkDir;

pub const MAX_FOLDERS: usize = 20;
pub const READ_CHUNK_CHARS: usize = 20_000;
const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;
/// Uncompressed size limit for one XML part of an Office file (zip-bomb guard).
const MAX_XML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES_SCANNED: usize = 20_000;
const MAX_HITS: usize = 10;
const MAX_TERMS: usize = 8;
const SNIPPET_BYTES: usize = 240;

/// Dependency, build and environment folders: large, generated, rarely what a meeting is about.
/// Hidden folders (".git", ".venv", ...) are skipped too.
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__", "venv"];

const SKIP_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "tif", "tiff", "heic", "psd", "mp3", "mp4", "wav",
    "m4a", "flac", "ogg", "mov", "avi", "mkv", "webm", "zip", "7z", "rar", "gz", "tgz", "bz2", "xz", "tar",
    "iso", "dmg", "exe", "dll", "so", "dylib", "bin", "o", "a", "lib", "obj", "class", "jar", "pyc", "wasm",
    "woff", "woff2", "ttf", "otf", "eot", "db", "sqlite", "sqlite3", "onnx", "gguf", "safetensors", "pt",
    "doc", "ppt", "xls",
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Pdf,
    Word,
    PowerPoint,
    Excel,
}

fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => Some(Kind::Pdf),
        "docx" | "docm" => Some(Kind::Word),
        "pptx" | "pptm" => Some(Kind::PowerPoint),
        "xlsx" | "xlsm" => Some(Kind::Excel),
        e if SKIP_EXTENSIONS.contains(&e) => None,
        _ => Some(Kind::Text),
    }
}

/// Path as shown to the user and the model (without Windows' `\\?\` prefix).
pub fn display(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&*s).to_string()
}

// ---------- allowed folders ----------

pub fn load_folders(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_folders(file: &Path, folders: &[String]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not save folders: {}", e))?;
    }
    let json = serde_json::to_string_pretty(folders).map_err(|e| e.to_string())?;
    std::fs::write(file, json).map_err(|e| format!("Could not save folders: {}", e))
}

/// `folders` plus `new`, unless it is already covered by the list.
pub fn add_folder(mut folders: Vec<String>, new: &Path) -> Result<Vec<String>, String> {
    let resolved = std::fs::canonicalize(new).map_err(|_| "That folder does not exist.".to_string())?;
    if !resolved.is_dir() {
        return Err("Please choose a folder.".into());
    }
    if resolve_roots(&folders).contains(&resolved) {
        return Ok(folders);
    }
    if folders.len() >= MAX_FOLDERS {
        return Err(format!("You can allow up to {} folders.", MAX_FOLDERS));
    }
    folders.push(display(&resolved));
    Ok(folders)
}

/// Allowed folders that still exist, resolved.
pub fn resolve_roots(folders: &[String]) -> Vec<PathBuf> {
    folders
        .iter()
        .filter_map(|f| std::fs::canonicalize(f).ok())
        .filter(|p| p.is_dir())
        .collect()
}

/// `path` resolved, if it lies inside one of `roots`.
pub fn resolve_inside(roots: &[PathBuf], path: &str) -> Result<PathBuf, String> {
    let resolved = std::fs::canonicalize(path.trim()).map_err(|_| format!("File not found: {}", path))?;
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(resolved)
    } else {
        Err("That file is outside the folders the copilot may read.".into())
    }
}

// ---------- text extraction ----------

fn read_text_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None; // binary
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn cached_regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn unescape_xml(s: &str) -> String {
    static ENTITY: OnceLock<Regex> = OnceLock::new();
    cached_regex(&ENTITY, r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos);")
        .replace_all(s, |c: &regex::Captures| match &c[1] {
            "amp" => "&".to_string(),
            "lt" => "<".to_string(),
            "gt" => ">".to_string(),
            "quot" => "\"".to_string(),
            "apos" => "'".to_string(),
            num => {
                let code = match num.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num[1..].parse().ok(),
                };
                code.and_then(char::from_u32).map(String::from).unwrap_or_default()
            }
        })
        .into_owned()
}

fn strip_tags(xml: &str) -> String {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    unescape_xml(&cached_regex(&TAGS, r"<[^>]*>").replace_all(xml, ""))
}

/// Text of WordprocessingML / DrawingML: one line per paragraph.
fn xml_paragraphs(xml: &str) -> String {
    static BREAKS: OnceLock<Regex> = OnceLock::new();
    static TABS: OnceLock<Regex> = OnceLock::new();
    let text = cached_regex(&BREAKS, r"</(?:w|a):p>|<(?:w|a):br\b[^>]*>").replace_all(xml, "\n");
    let text = cached_regex(&TABS, r"<w:tab\b[^>]*>").replace_all(&text, " ");
    strip_tags(&text)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// XML parts of an Office (zip) file whose names pass `want`, in natural order
/// (slide2.xml before slide10.xml).
fn zip_parts(path: &Path, want: impl Fn(&str) -> bool) -> Option<Vec<(String, String)>> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut names: Vec<String> = archive.file_names().filter(|n| want(n)).map(str::to_string).collect();
    names.sort_by_key(|n| (n.len(), n.clone()));
    let mut parts = Vec::new();
    for name in names {
        let mut bytes = Vec::new();
        archive.by_name(&name).ok()?.take(MAX_XML_BYTES).read_to_end(&mut bytes).ok()?;
        parts.push((name, String::from_utf8_lossy(&bytes).into_owned()));
    }
    Some(parts)
}

fn excel_text(path: &Path) -> Option<String> {
    static SHARED: OnceLock<Regex> = OnceLock::new();
    static ROW: OnceLock<Regex> = OnceLock::new();
    static CELL: OnceLock<Regex> = OnceLock::new();
    static VALUE: OnceLock<Regex> = OnceLock::new();
    static INLINE: OnceLock<Regex> = OnceLock::new();
    let parts = zip_parts(path, |n| {
        n == "xl/sharedStrings.xml" || (n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
    })?;

    let shared: Vec<String> = parts
        .iter()
        .find(|(n, _)| n == "xl/sharedStrings.xml")
        .map(|(_, xml)| {
            cached_regex(&SHARED, r"(?s)<si\b[^>]*>(.*?)</si>")
                .captures_iter(xml)
                .map(|c| strip_tags(&c[1]))
                .collect()
        })
        .unwrap_or_default();

    let mut out = String::new();
    for (i, (_, xml)) in parts.iter().filter(|(n, _)| n.starts_with("xl/worksheets/")).enumerate() {
        out.push_str(&format!("[Sheet {}]\n", i + 1));
        for row in cached_regex(&ROW, r"(?s)<row\b[^>]*>(.*?)</row>").captures_iter(xml) {
            let cells: Vec<String> = cached_regex(&CELL, r"(?s)<c\b([^>]*?)(?:/>|>(.*?)</c>)")
                .captures_iter(&row[1])
                .filter_map(|c| {
                    let attrs = c.get(1).map_or("", |m| m.as_str());
                    let body = c.get(2)?.as_str();
                    if let Some(v) = cached_regex(&VALUE, r"(?s)<v>(.*?)</v>").captures(body) {
                        if attrs.contains(r#"t="s""#) {
                            return v[1].trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned());
                        }
                        return Some(unescape_xml(&v[1]));
                    }
                    cached_regex(&INLINE, r"(?s)<is>(.*?)</is>").captures(body).map(|t| strip_tags(&t[1]))
                })
                .filter(|v| !v.trim().is_empty())
                .collect();
            if !cells.is_empty() {
                out.push_str(&cells.join("\t"));
                out.push('\n');
            }
        }
    }
    Some(out)
}

fn extract_document(path: &Path, kind: Kind) -> Option<String> {
    match kind {
        Kind::Text => read_text_file(path),
        Kind::Pdf => {
            // pdf-extract can panic on malformed files; one bad PDF must not break a search.
            let path = path.to_path_buf();
            std::panic::catch_unwind(move || pdf_extract::extract_text(&path).ok()).ok().flatten()
        }
        Kind::Word => {
            let parts = zip_parts(path, |n| n == "word/document.xml")?;
            Some(parts.iter().map(|(_, xml)| xml_paragraphs(xml)).collect::<Vec<_>>().join("\n"))
        }
        Kind::PowerPoint => {
            let parts = zip_parts(path, |n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))?;
            Some(
                parts
                    .iter()
                    .enumerate()
                    .map(|(i, (_, xml))| format!("[Slide {}]\n{}", i + 1, xml_paragraphs(xml)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        Kind::Excel => excel_text(path),
    }
}

type TextCache = Mutex<HashMap<PathBuf, (SystemTime, u64, Arc<str>)>>;

fn document_cache() -> &'static TextCache {
    static CACHE: OnceLock<TextCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Text of a supported file; None if unsupported, too large or unreadable.
/// A document whose text cannot be extracted (e.g. a scanned PDF) gives empty text.
fn file_text(path: &Path) -> Option<Arc<str>> {
    let kind = kind_of(path)?;
    let meta = std::fs::metadata(path).ok()?;
    let limit = if kind == Kind::Text { MAX_TEXT_FILE_BYTES } else { MAX_DOCUMENT_BYTES };
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    if kind == Kind::Text {
        return read_text_file(path).map(Arc::from);
    }

    // Documents are slow to extract, so keep their text until the file changes.
    let modified = meta.modified().ok()?;
    if let Some((m, len, text)) = document_cache().lock().ok()?.get(path) {
        if *m == modified && *len == meta.len() {
            return Some(text.clone());
        }
    }
    let text: Arc<str> = Arc::from(extract_document(path, kind).unwrap_or_default());
    document_cache().lock().ok()?.insert(path.to_path_buf(), (modified, meta.len(), text.clone()));
    Some(text)
}

// ---------- search and read ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub snippet: String,
}

fn skip_dir(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref())
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn snippet_around(text: &str, at: usize) -> String {
    let start = floor_boundary(text, at.saturating_sub(SNIPPET_BYTES / 2));
    let end = floor_boundary(text, (at + SNIPPET_BYTES / 2).min(text.len()));
    text[start..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Files under `roots` whose name or text contains the query's keywords, best first:
/// more distinct keywords matched, then keywords in the file name, then more occurrences.
pub fn search(roots: &[PathBuf], query: &str) -> Vec<Hit> {
    let mut terms: Vec<(String, Regex)> = Vec::new();
    for word in query.split_whitespace() {
        let term = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        if term.chars().count() < 2 || terms.iter().any(|(t, _)| *t == term) || terms.len() >= MAX_TERMS {
            continue;
        }
        if let Ok(re) = Regex::new(&format!("(?i){}", regex::escape(&term))) {
            terms.push((term, re));
        }
    }
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<((usize, usize, usize), Hit)> = Vec::new();
    let mut scanned = 0;
    for root in roots {
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_type().is_dir() || !skip_dir(e.file_name()));
        for entry in walker.filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            scanned += 1;
            if scanned > MAX_FILES_SCANNED {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let text = file_text(entry.path());
            let (mut matched, mut in_name, mut count, mut first) = (0, 0, 0, None);
            for (term, re) in &terms {
                let name_hit = name.contains(term.as_str());
                let mut n = 0;
                if let Some(text) = &text {
                    for m in re.find_iter(text).take(1000) {
                        first.get_or_insert(m.start());
                        n += 1;
                    }
                }
                if name_hit || n > 0 {
                    matched += 1;
                }
                if name_hit {
                    in_name += 1;
                }
                count += n;
            }
            if matched == 0 {
                continue;
            }
            let snippet = match (&text, first) {
                (Some(text), Some(at)) => snippet_around(text, at),
                _ => String::new(),
            };
            scored.push(((matched, in_name, count), Hit { path: display(entry.path()), snippet }));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(MAX_HITS).map(|(_, hit)| hit).collect()
}

/// Up to READ_CHUNK_CHARS characters of a file's text from `start`, with a note when more remains.
/// Returns the file's display path and the text.
pub fn read(roots: &[PathBuf], path: &str, start: usize) -> Result<(String, String), String> {
    let path = resolve_inside(roots, path)?;
    let text = file_text(&path).ok_or("This file type cannot be read, or the file is too large.")?;
    if text.trim().is_empty() {
        return Err("No readable text in this file (it may be a scanned document or image).".into());
    }
    let total = text.chars().count();
    let mut out: String = text.chars().skip(start).take(READ_CHUNK_CHARS).collect();
    let end = start + out.chars().count();
    if end < total {
        out.push_str(&format!(
            "\n\n[Showing characters {}-{} of {}. Call read_file with start={} for more.]",
            start, end, total, end
        ));
    }
    Ok((display(&path), out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meetily-copilot-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_zip(path: &Path, parts: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A one-page PDF showing `text` in Helvetica, with a correct xref table.
    fn write_pdf(path: &Path, text: &str) {
        let stream = format!("BT /F1 24 Tf 72 720 Td ({}) Tj ET", text);
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", stream.len(), stream),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
        }
        let xref = pdf.len();
        pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for offset in offsets {
            pdf.push_str(&format!("{:010} 00000 n \n", offset));
        }
        pdf.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", objects.len() + 1, xref));
        std::fs::write(path, pdf).unwrap();
    }

    #[test]
    fn file_kinds() {
        assert_eq!(kind_of(Path::new("a/notes.MD")), Some(Kind::Text));
        assert_eq!(kind_of(Path::new("Makefile")), Some(Kind::Text));
        assert_eq!(kind_of(Path::new("deck.pptx")), Some(Kind::PowerPoint));
        assert_eq!(kind_of(Path::new("photo.JPG")), None);
        assert_eq!(kind_of(Path::new("old.doc")), None);
    }

    #[test]
    fn xml_helpers() {
        assert_eq!(unescape_xml("a &amp; b &lt;c&gt; &#233;&#x41;"), "a & b <c> éA");
        let xml = r#"<w:body><w:p><w:r><w:t>Hello</w:t><w:tab/><w:t xml:space="preserve">world</w:t></w:r></w:p><w:p><w:r><w:t>R&amp;D</w:t></w:r></w:p></w:body>"#;
        assert_eq!(xml_paragraphs(xml), "Hello world\nR&D");
    }

    #[test]
    fn extracts_office_and_pdf_text() {
        let dir = temp_dir("docs");
        let docx = dir.join("proposal.docx");
        write_zip(&docx, &[("word/document.xml", "<w:p><w:r><w:t>Acme pricing</w:t></w:r></w:p>")]);
        assert_eq!(file_text(&docx).as_deref(), Some("Acme pricing"));

        let pptx = dir.join("deck.pptx");
        write_zip(&pptx, &[
            ("ppt/slides/slide10.xml", "<a:p><a:r><a:t>Ten</a:t></a:r></a:p>"),
            ("ppt/slides/slide2.xml", "<a:p><a:r><a:t>Two</a:t></a:r></a:p>"),
            ("ppt/slides/_rels/slide2.xml.rels", "<x>ignored</x>"),
        ]);
        assert_eq!(file_text(&pptx).as_deref(), Some("[Slide 1]\nTwo\n[Slide 2]\nTen"));

        let xlsx = dir.join("numbers.xlsx");
        write_zip(&xlsx, &[
            ("xl/sharedStrings.xml", "<sst><si><t>Revenue</t></si><si><r><t>Q3</t></r></si></sst>"),
            (
                "xl/worksheets/sheet1.xml",
                r#"<sheetData><row r="1"><c r="A1" t="s"><v>1</v></c><c r="B1" t="s"><v>0</v></c></row><row r="2"><c r="A2"><v>1250.5</v></c><c r="B2" t="inlineStr"><is><t>est.</t></is></c><c r="C2"/></row></sheetData>"#,
            ),
        ]);
        assert_eq!(file_text(&xlsx).as_deref(), Some("[Sheet 1]\nQ3\tRevenue\n1250.5\test.\n"));

        let pdf = dir.join("report.pdf");
        write_pdf(&pdf, "Quarterly revenue grew");
        assert!(file_text(&pdf).unwrap().contains("Quarterly revenue grew"));

        // A broken PDF gives empty text instead of an error or a panic.
        let broken = dir.join("broken.pdf");
        std::fs::write(&broken, "%PDF-1.4 not really").unwrap();
        assert_eq!(file_text(&broken).as_deref(), Some(""));
    }

    #[test]
    fn searches_and_reads_only_inside_allowed_folders() {
        let dir = temp_dir("search");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("notes.md"), "Meeting notes.\nAcme wants the pricing proposal by Friday.").unwrap();
        std::fs::write(dir.join("src/acme_client.rs"), "fn call_acme() {}").unwrap();
        std::fs::write(dir.join("node_modules/pkg/acme.md"), "acme pricing").unwrap();
        std::fs::write(dir.join(".git/acme"), "acme pricing").unwrap();
        std::fs::write(dir.join("logo.png"), "acme pricing").unwrap();
        std::fs::write(dir.join("binary.dat"), b"acme pricing\0\0").unwrap();
        let outside = temp_dir("outside").join("secret.txt");
        std::fs::write(&outside, "acme pricing").unwrap();

        let roots = resolve_roots(&[display(&dir)]);
        let hits = search(&roots, "Acme pricing?");
        let names: Vec<String> = hits
            .iter()
            .map(|h| Path::new(&h.path).file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["notes.md", "acme_client.rs"]);
        assert!(hits[0].snippet.contains("Acme wants the pricing proposal"));
        assert!(search(&roots, "a ?").is_empty());

        let (path, text) = read(&roots, &hits[0].path, 0).unwrap();
        assert!(path.ends_with("notes.md"));
        assert!(text.starts_with("Meeting notes."));
        let (_, rest) = read(&roots, &hits[0].path, 15).unwrap();
        assert!(rest.starts_with("Acme wants"));

        assert!(read(&roots, &display(&outside), 0).is_err());
        let sneaky = format!("{}/src/../../{}", display(&dir), outside.parent().unwrap().file_name().unwrap().to_string_lossy());
        assert!(read(&roots, &format!("{}/secret.txt", sneaky), 0).is_err());
        assert!(read(&roots, &display(&dir.join("missing.md")), 0).is_err());
    }

    #[test]
    fn long_files_are_read_in_chunks() {
        let dir = temp_dir("chunks");
        let file = dir.join("long.txt");
        std::fs::write(&file, "x".repeat(READ_CHUNK_CHARS + 5)).unwrap();
        let roots = resolve_roots(&[display(&dir)]);
        let (_, first) = read(&roots, &display(&file), 0).unwrap();
        assert!(first.ends_with(&format!("Call read_file with start={} for more.]", READ_CHUNK_CHARS)));
        let (_, last) = read(&roots, &display(&file), READ_CHUNK_CHARS).unwrap();
        assert_eq!(last, "xxxxx");
    }

    #[test]
    fn folder_list_management() {
        let dir = temp_dir("folders");
        let folders = add_folder(Vec::new(), &dir).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(add_folder(folders.clone(), &dir).unwrap(), folders); // no duplicate
        assert!(add_folder(folders.clone(), &dir.join("nope")).is_err());

        let file = dir.join("settings/copilot-folders.json");
        assert!(load_folders(&file).is_empty());
        save_folders(&file, &folders).unwrap();
        assert_eq!(load_folders(&file), folders);
    }
}
