//! Text encodings for open and save: sniff a file's encoding on open, encode
//! it back the way it came on save. Modeled on Zed's `file_content` crate.

use std::{borrow::Cow, io::{self, Read}, path::Path};
use std::os::unix::fs::OpenOptionsExt;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use gpui_kit::component::input::Rope;

/// How much of the file is looked at to tell binary from BOM-less UTF-16.
const SNIFF_LEN: usize = 8 * 1024;
const O_NONBLOCK: i32 = 0x0004; // Darwin fcntl.h

/// Opening more than 256 MiB needs an explicit choice before the body is read.
pub const OPEN_LIMIT: u64 = 256 * 1024 * 1024;

pub struct Decoded {
    pub text: String,
    pub encoding: &'static Encoding,
    pub has_bom: bool,
    /// Decoding changed bytes that a save must not silently replace.
    pub replaced: bool,
}

impl Decoded {
    /// Text that is plain UTF-8 with no BOM, as a new page or a draft holds it.
    pub fn utf8(text: String) -> Self {
        Self { text, encoding: UTF_8, has_bom: false, replaced: false }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Binary,
}

/// Reads the file at `path` and decodes it; a binary file is an `InvalidData` error.
pub fn read_file(path: &Path) -> io::Result<Decoded> {
    decode(read_bytes(path, false)?).map_err(|DecodeError::Binary| io::Error::new(io::ErrorKind::InvalidData, "it isn’t a text file"))
}

/// The same read after the user has agreed to open a large file.
pub fn read_large_file(path: &Path) -> io::Result<Decoded> {
    decode(read_bytes(path, true)?).map_err(|DecodeError::Binary| io::Error::new(io::ErrorKind::InvalidData, "it isn’t a text file"))
}

/// The large-file question's title and detail, using Finder's decimal units.
pub fn large_file_question(path: &Path) -> (String, String) {
    let bytes = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    let size = if bytes >= 1_000_000_000 { format!("{:.1} GB", bytes as f64 / 1_000_000_000.) } else { format!("{:.0} MB", bytes as f64 / 1_000_000.) };
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    (format!("Open “{name}”?"), format!("It’s {size}. Opening it may take a while and use a lot of memory."))
}

/// Refuse special files before opening them, and reject binary data before reading its body.
fn read_bytes(path: &Path, allow_large: bool) -> io::Result<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    // A regular path can become a pipe between stat and open; never wait for a writer.
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut bytes = Vec::new();
    file.by_ref().take(SNIFF_LEN as u64).read_to_end(&mut bytes)?;
    if is_utf32le_bom(&bytes) || (Encoding::for_bom(&bytes).is_none() && bytes.contains(&0) && bomless_utf16(&bytes).is_none()) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "it isn’t a text file"));
    }
    if !allow_large && metadata.len() > OPEN_LIMIT {
        return Err(io::Error::new(io::ErrorKind::FileTooLarge, "opening this file needs confirmation"));
    }
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Reads a regular file whole and decodes it as `encoding` whatever it holds (`decode_as`), for
/// File > Reopen with encoding. Special files are refused as in `read_bytes`; binary data is not.
pub fn read_file_as(path: &Path, encoding: &'static Encoding) -> io::Result<Decoded> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(decode_as(&bytes, encoding))
}

/// A UTF-32LE byte order mark (`FF FE 00 00`) starts with the UTF-16LE one; Finch opens neither UTF-32 as text.
fn is_utf32le_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00])
}

/// Reads the file at `path` again for a document in `current`: bytes `current` still reads
/// cleanly stay in it (our own save, or a file that is a wide encoding's text with a NUL in it, or
/// text left ASCII-only in an ASCII-based one), unless a BOM, UTF-8 beyond ASCII, a NUL in an
/// ASCII-based encoding, or an all-ASCII file under a wide encoding says otherwise; anything else
/// decodes as `read_file` does.
pub fn reread_file(path: &Path, current: &'static Encoding) -> io::Result<Decoded> {
    let bytes = read_bytes(path, true)?;
    let utf8_beyond_ascii = !bytes.is_ascii() && std::str::from_utf8(&bytes).is_ok();
    let sample = &bytes[..bytes.len().min(SNIFF_LEN)];
    let nul = current.is_ascii_compatible() && sample.contains(&0);
    // A wide encoding over a file that is all ASCII is not what was written: Latin text in it always has NULs.
    let wide_without_nul = !current.is_ascii_compatible() && !sample.contains(&0) && bytes.is_ascii();
    if current != UTF_8 && Encoding::for_bom(&bytes).is_none() && !utf8_beyond_ascii && !nul && !wide_without_nul {
        if let Some(text) = current.decode_without_bom_handling_and_without_replacement(&bytes) {
            let text = text.into_owned();
            let replaced = encode(&text, current, false).0.as_ref() != bytes.as_slice();
            return Ok(Decoded { text, encoding: current, has_bom: false, replaced });
        }
    }
    decode(bytes).map_err(|DecodeError::Binary| io::Error::new(io::ErrorKind::InvalidData, "it isn’t a text file"))
}

/// The most appended bytes one tick reads and applies at once; more than this takes the full reload.
const TAIL_MAX: usize = 8 * 1024 * 1024;

/// How much of the end of the old text a bounded growth check compares.
const GROW_CHECK: usize = 4096;

/// The old text of a followed file bigger than this is not compared whole every tick (see `GrowBase::full`).
pub const GROW_FULL_MAX: usize = 8 * 1024 * 1024;

/// What a document holds of a UTF-8 file on disk, for telling a file that only grew from one that
/// changed: `len` bytes of `text` after a `bom`-long byte order mark. `full` asks for every old
/// byte to be compared; otherwise only the last `GROW_CHECK` bytes are, and the file must be the
/// one (device and inode) `ident` names, which an earlier whole compare or whole read recorded.
pub struct GrowBase {
    pub len: usize,
    pub bom: usize,
    pub text: Rope,
    pub full: bool,
    pub ident: Option<(u64, u64)>,
}

/// What the file gained past a `GrowBase`: `text` (empty when the file is as it was), the bytes
/// `read` to get it, the bytes the file holds now and the file's device and inode.
pub struct Tail {
    /// The `GrowBase::len` this text goes after.
    pub base: usize,
    pub text: String,
    pub read: usize,
    pub disk_len: u64,
    pub ident: (u64, u64),
}

/// Compares a file's bytes, read in 64 KB blocks, with the pieces of old text handed to `matches`.
struct Compare<'a> {
    file: &'a mut std::fs::File,
    block: Vec<u8>,
    at: usize,
    filled: usize,
}

impl Compare<'_> {
    /// Whether the next bytes of the file are `old`. A file that ends first is a mismatch.
    fn matches(&mut self, mut old: &[u8]) -> io::Result<bool> {
        while !old.is_empty() {
            if self.at == self.filled {
                self.filled = self.file.read(&mut self.block)?;
                self.at = 0;
                if self.filled == 0 {
                    return Ok(false);
                }
            }
            let n = old.len().min(self.filled - self.at);
            if self.block[self.at..self.at + n] != old[..n] {
                return Ok(false);
            }
            self.at += n;
            old = &old[n..];
        }
        Ok(true)
    }
}

/// The last `n` bytes of `text`.
fn last_bytes(text: &Rope, n: usize) -> Vec<u8> {
    let len = text.len();
    let want = len.min(n);
    let mut pieces: Vec<&str> = Vec::new();
    let mut seen = 0;
    let (mut chunks, _) = text.chunks_at(len);
    while seen < want {
        let Some(chunk) = chunks.prev() else { break };
        seen += chunk.len();
        pieces.push(chunk);
    }
    let mut out = Vec::with_capacity(seen);
    for piece in pieces.iter().rev() {
        out.extend_from_slice(piece.as_bytes());
    }
    out.drain(..out.len() - want);
    out
}

/// Reads only the bytes `path` gained past `base`. `Ok(None)` when the file is not simply the
/// old one plus more (shorter, an old byte differs, another file under the name, a NUL, bytes that
/// aren't UTF-8, an empty old file or more than `TAIL_MAX` new): the caller reads it whole. With
/// `base.full` every old byte is compared with the file here, off the main thread, so a change
/// anywhere is seen; otherwise the end of the old text and the file's identity are. A character
/// cut short at the end of what the writer has flushed so far waits for the next read.
pub fn read_tail(path: &Path, base: &GrowBase) -> io::Result<Option<Tail>> {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    if base.len == 0 {
        return Ok(None);
    }
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(path)?;
    let metadata = file.metadata()?;
    let at = (base.bom + base.len) as u64;
    if !metadata.is_file() || metadata.len() <= at {
        return Ok(None);
    }
    let ident = (metadata.dev(), metadata.ino());
    let check = last_bytes(&base.text, GROW_CHECK);
    let mut compare = Compare { file: &mut file, block: vec![0; 64 * 1024], at: 0, filled: 0 };
    if base.full {
        compare.file.seek(SeekFrom::Start(base.bom as u64))?;
        for chunk in base.text.chunks() {
            if !compare.matches(chunk.as_bytes())? {
                return Ok(None);
            }
        }
    } else {
        if base.ident != Some(ident) {
            return Ok(None);
        }
        compare.file.seek(SeekFrom::Start(at - check.len() as u64))?;
        if !compare.matches(&check)? {
            return Ok(None);
        }
    }
    file.seek(SeekFrom::Start(at))?;
    let mut bytes = Vec::new();
    file.by_ref().take(TAIL_MAX as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > TAIL_MAX || bytes.contains(&0) {
        return Ok(None);
    }
    let read = bytes.len();
    let valid = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(err) if err.error_len().is_none() => err.valid_up_to(),
        Err(_) => return Ok(None),
    };
    bytes.truncate(valid);
    let text = String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "it isn’t a text file"))?;
    // A CR that may be half of a CRLF still to come, or a bare CR, takes the full read.
    if text.contains('\r') && endings(&text) != Endings::Crlf || check.last() == Some(&b'\r') {
        return Ok(None);
    }
    Ok(Some(Tail { base: base.len, text, read, disk_len: metadata.len(), ident }))
}

/// The encodings File > Reopen with encoding and File > Convert to list, in menu order.
pub const REOPEN_CHOICES: [&Encoding; 12] = [
    UTF_8,
    UTF_16LE,
    UTF_16BE,
    encoding_rs::MACINTOSH,
    encoding_rs::WINDOWS_1252,
    encoding_rs::WINDOWS_1250,
    encoding_rs::WINDOWS_1251,
    encoding_rs::SHIFT_JIS,
    encoding_rs::EUC_JP,
    encoding_rs::GBK,
    encoding_rs::EUC_KR,
    encoding_rs::BIG5,
];

/// The labelled encodings the short list leaves out, in the Mac's order (region by region): the
/// More encodings submenu of File > Reopen with encoding and File > Convert to, and of the status line's encoding menu.
pub const MORE_CHOICES: [&Encoding; 21] = [
    encoding_rs::ISO_8859_15,
    encoding_rs::ISO_8859_2,
    encoding_rs::ISO_8859_3,
    encoding_rs::ISO_8859_4,
    encoding_rs::ISO_8859_5,
    encoding_rs::KOI8_R,
    encoding_rs::KOI8_U,
    encoding_rs::X_MAC_CYRILLIC,
    encoding_rs::IBM866,
    encoding_rs::ISO_8859_6,
    encoding_rs::WINDOWS_1256,
    encoding_rs::ISO_8859_7,
    encoding_rs::WINDOWS_1253,
    encoding_rs::ISO_8859_8,
    encoding_rs::WINDOWS_1255,
    encoding_rs::WINDOWS_1254,
    encoding_rs::WINDOWS_1257,
    encoding_rs::WINDOWS_1258,
    encoding_rs::WINDOWS_874,
    encoding_rs::ISO_2022_JP,
    encoding_rs::GB18030,
];

/// Decodes `bytes` as `encoding` whatever they look like, as File > Reopen with encoding does. A BOM
/// that belongs to `encoding` is dropped and counted; `replaced` is true when writing the text back
/// would not restore `bytes`: some had no character in `encoding` (they came out as U+FFFD), or
/// `encoding` writes some character as other bytes (Shift JIS vendor extensions, GBK A2E3).
pub fn decode_as(bytes: &[u8], encoding: &'static Encoding) -> Decoded {
    let (body, has_bom) = match Encoding::for_bom(bytes) {
        Some((found, len)) if found == encoding => (&bytes[len..], true),
        _ => (bytes, false),
    };
    let (text, replaced) = encoding.decode_without_bom_handling(body);
    let text = text.into_owned();
    let lossy = replaced || encode(&text, encoding, has_bom).0.as_ref() != bytes;
    Decoded { text, encoding, has_bom, replaced: lossy }
}

/// Valid UTF-8 is taken as it is, never guessed at; a BOM or a UTF-16 pattern names its encoding;
/// anything else is guessed from the bytes.
pub fn decode(bytes: Vec<u8>) -> Result<Decoded, DecodeError> {
    if is_utf32le_bom(&bytes) {
        return Err(DecodeError::Binary);
    }
    if let Some((encoding, bom_len)) = Encoding::for_bom(&bytes) {
        let (text, replaced) = encoding.decode_without_bom_handling(&bytes[bom_len..]);
        return Ok(Decoded { text: text.into_owned(), encoding, has_bom: true, replaced });
    }

    let sample = &bytes[..bytes.len().min(SNIFF_LEN)];
    if sample.contains(&0) {
        // Binary unless the NULs sit on one parity of offsets, which is
        // ASCII-heavy UTF-16 without a BOM.
        let encoding = bomless_utf16(sample).ok_or(DecodeError::Binary)?;
        return encoding
            .decode_without_bom_handling_and_without_replacement(&bytes)
            .map(|text| Decoded { text: text.into_owned(), encoding, has_bom: false, replaced: false })
            .ok_or(DecodeError::Binary);
    }

    let bytes = match String::from_utf8(bytes) {
        Ok(text) => return Ok(Decoded::utf8(text)),
        Err(err) => err.into_bytes(),
    };

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(&bytes, true);
    let encoding = detector.guess(None, true);
    let (text, replaced) = encoding.decode_without_bom_handling(&bytes);
    let text = text.into_owned();
    // A guess that writes some character as other bytes than it was read from would change lines nobody edited.
    let replaced = replaced || encode(&text, encoding, false).0.as_ref() != bytes.as_slice();
    Ok(Decoded { text, encoding, has_bom: false, replaced })
}

/// UTF-16LE puts the NULs of Latin text at odd offsets, UTF-16BE at even ones.
fn bomless_utf16(sample: &[u8]) -> Option<&'static Encoding> {
    let pairs = sample.len() / 2;
    if pairs == 0 {
        return None;
    }
    let (mut even, mut odd) = (0usize, 0usize);
    for (i, byte) in sample.iter().enumerate() {
        if *byte == 0 {
            if i % 2 == 0 {
                even += 1;
            } else {
                odd += 1;
            }
        }
    }
    let heavy = |a: usize, b: usize| a * 10 >= pairs * 4 && b * 20 <= pairs;
    if heavy(odd, even) {
        Some(UTF_16LE)
    } else if heavy(even, odd) {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// The kind of line break a text has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endings {
    /// No break, or only bare LF (a lone CR counts for neither kind).
    Lf,
    /// At least one break and every one is CRLF (no bare LF, no bare CR).
    Crlf,
    /// Both CRLF and bare LF breaks.
    Mixed,
}

/// Which line breaks `text` has, in one pass.
pub fn endings(text: &str) -> Endings {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf, mut cr) = (false, false, false);
    let mut at = 0;
    while let Some(found) = bytes[at..].iter().position(|&b| b == b'\n' || b == b'\r') {
        let i = at + found;
        if bytes[i] == b'\n' {
            lf = true;
            at = i + 1;
        } else if bytes.get(i + 1) == Some(&b'\n') {
            crlf = true;
            at = i + 2;
        } else {
            cr = true;
            at = i + 1;
        }
        if crlf && lf {
            return Endings::Mixed;
        }
    }
    if crlf && !lf && !cr { Endings::Crlf } else { Endings::Lf }
}

/// `text` with every CRLF written as a bare LF (a lone CR stays). Borrowed when there is no CRLF.
pub fn to_lf(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let crlf = |i: usize| bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n');
    let Some(first) = (0..bytes.len()).find(|&i| crlf(i)) else {
        return Cow::Borrowed(text);
    };
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    let mut i = first;
    loop {
        out.push_str(&text[start..i]);
        start = i + 1;
        match (start..bytes.len()).find(|&j| crlf(j)) {
            Some(next) => i = next,
            None => break,
        }
    }
    out.push_str(&text[start..]);
    Cow::Owned(out)
}

/// `text` with every bare LF written as CRLF; a CRLF already there stays one. Borrowed when
/// there is no bare LF.
pub fn to_crlf(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let bare = |i: usize| bytes[i] == b'\n' && (i == 0 || bytes[i - 1] != b'\r');
    if !(0..bytes.len()).any(bare) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + text.len() / 32);
    let mut start = 0;
    for i in (0..bytes.len()).filter(|&i| bare(i)) {
        out.push_str(&text[start..i]);
        out.push('\r');
        start = i;
    }
    out.push_str(&text[start..]);
    Cow::Owned(out)
}

/// Encodes `text` for saving. The flag is true when some character has no
/// form in `encoding` (the bytes then hold substitutes), so the caller can
/// refuse to lose it silently. UTF-8 and UTF-16 never lose characters.
pub fn encode<'a>(text: &'a str, encoding: &'static Encoding, has_bom: bool) -> (Cow<'a, [u8]>, bool) {
    if encoding == UTF_8 {
        if !has_bom {
            return (Cow::Borrowed(text.as_bytes()), false);
        }
        let mut out = Vec::with_capacity(3 + text.len());
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        out.extend_from_slice(text.as_bytes());
        return (Cow::Owned(out), false);
    }
    if encoding == UTF_16LE || encoding == UTF_16BE {
        let little = encoding == UTF_16LE;
        let mut out = Vec::with_capacity(2 + text.len() * 2);
        let mut push = |unit: u16| {
            out.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
        };
        if has_bom {
            push(0xFEFF);
        }
        for unit in text.encode_utf16() {
            push(unit);
        }
        return (Cow::Owned(out), false);
    }
    let (bytes, _, had_unmappable) = encoding.encode(text);
    (bytes, had_unmappable)
}

/// The Mac name for an encoding the app can open (`Western (Windows Latin 1)`), the raw
/// name for any other.
pub fn label(encoding: &'static Encoding) -> Cow<'static, str> {
    let name = match encoding.name() {
        "UTF-8" => "Unicode (UTF-8)",
        "UTF-16LE" => "Unicode (UTF-16 LE)",
        "UTF-16BE" => "Unicode (UTF-16 BE)",
        "windows-1252" => "Western (Windows Latin 1)",
        "macintosh" => "Western (Mac OS Roman)",
        "ISO-8859-1" => "Western (ISO Latin 1)",
        "ISO-8859-15" => "Western (ISO Latin 9)",
        "ISO-8859-2" => "Central European (ISO Latin 2)",
        "windows-1250" => "Central European (Windows Latin 2)",
        "ISO-8859-3" => "Western (ISO Latin 3)",
        "ISO-8859-4" => "Baltic (ISO Latin 4)",
        "ISO-8859-5" => "Cyrillic (ISO 8859-5)",
        "windows-1251" => "Cyrillic (Windows)",
        "KOI8-R" => "Cyrillic (KOI8-R)",
        "KOI8-U" => "Cyrillic (KOI8-U)",
        "x-mac-cyrillic" => "Cyrillic (Mac OS)",
        "IBM866" => "Cyrillic (DOS)",
        "ISO-8859-6" => "Arabic (ISO 8859-6)",
        "windows-1256" => "Arabic (Windows)",
        "ISO-8859-7" => "Greek (ISO 8859-7)",
        "windows-1253" => "Greek (Windows)",
        "ISO-8859-8" | "ISO-8859-8-I" => "Hebrew (ISO 8859-8)",
        "windows-1255" => "Hebrew (Windows)",
        "windows-1254" => "Turkish (Windows Latin 5)",
        "windows-1257" => "Baltic (Windows)",
        "windows-1258" => "Vietnamese (Windows)",
        "windows-874" => "Thai (Windows)",
        "Shift_JIS" => "Japanese (Shift JIS)",
        "EUC-JP" => "Japanese (EUC)",
        "ISO-2022-JP" => "Japanese (ISO 2022-JP)",
        "EUC-KR" => "Korean (EUC)",
        "GBK" => "Chinese (GBK)",
        "gb18030" => "Chinese (GB 18030)",
        "Big5" => "Chinese (Big 5)",
        _ => return Cow::Borrowed(encoding.name()),
    };
    Cow::Borrowed(name)
}

/// The first character of `text` that `encoding` has no form for, if any.
pub fn first_unmappable(text: &str, encoding: &'static Encoding) -> Option<char> {
    let mut encoder = encoding.output_encoding().new_encoder();
    let mut buf = [0u8; 4096];
    let mut rest = text;
    loop {
        let (result, read, _) = encoder.encode_from_utf8_without_replacement(rest, &mut buf, true);
        rest = &rest[read..];
        match result {
            encoding_rs::EncoderResult::InputEmpty => return None,
            encoding_rs::EncoderResult::OutputFull => {}
            encoding_rs::EncoderResult::Unmappable(ch) => return Some(ch),
        }
    }
}

/// The failed-encoding question's title and detail for the file called `name`, read as `label`.
/// `replaced`: the file had bytes the decoder replaced; otherwise the text now has `culprit` or
/// other characters the encoding can't hold.
pub fn lossy_question(name: &str, label: &str, replaced: bool, culprit: Option<char>) -> (String, String) {
    if replaced {
        (
            format!("“{name}” won’t save back as it was read"),
            format!("It was read as {label}. Some bytes have no character in that encoding, or would be written differently. Reopen with another encoding to read them, or save as UTF-8 to keep the text as shown. Cancel leaves the file as it is."),
        )
    } else {
        (
            format!("Some characters can’t be saved in {label}"),
            match culprit {
                Some(ch) => format!("The first is “{ch}”. Saving as UTF-8 keeps every character. Cancel leaves the file as it is."),
                None => "Saving them as UTF-8 keeps every character. Cancel leaves the file as it is.".to_string(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_long_list_covers_every_labelled_encoding_once_and_leaves_the_short_list_out() {
        use encoding_rs::*;
        let known: [&'static Encoding; 39] = [
            BIG5, EUC_JP, EUC_KR, GB18030, GBK, IBM866, ISO_2022_JP, ISO_8859_2, ISO_8859_3, ISO_8859_4, ISO_8859_5, ISO_8859_6, ISO_8859_7,
            ISO_8859_8, ISO_8859_8_I, ISO_8859_10, ISO_8859_13, ISO_8859_14, ISO_8859_15, ISO_8859_16, KOI8_R, KOI8_U, MACINTOSH, SHIFT_JIS,
            UTF_16BE, UTF_16LE, UTF_8, WINDOWS_874, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255,
            WINDOWS_1256, WINDOWS_1257, WINDOWS_1258, X_MAC_CYRILLIC, REPLACEMENT,
        ];
        let short: Vec<&Encoding> = REOPEN_CHOICES.to_vec();
        let more: Vec<&Encoding> = MORE_CHOICES.to_vec();
        assert!(more.iter().all(|encoding| !short.contains(encoding)), "the short list's items repeat in the long one");
        let mut labels: Vec<String> = short.iter().chain(&more).map(|&encoding| label(encoding).into_owned()).collect();
        let listed = labels.len();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), listed, "an encoding is listed twice");
        for encoding in known {
            let named = label(encoding);
            if named != encoding.name() {
                assert!(labels.contains(&named.into_owned()), "{} has a label and no menu row", encoding.name());
            }
        }
        assert_eq!(listed, 33);
        let (bytes, lost) = encode("\u{3042}", ISO_2022_JP, false);
        assert!(!lost && bytes.as_ref() == b"\x1b$B$\"\x1b(B", "{bytes:?}");
    }

    #[test]
    fn the_large_file_question_has_a_short_title_and_the_size_in_the_detail() {
        let path = std::env::temp_dir().join(format!("finch-large-q-{}.log", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(300_000_000).unwrap();
        drop(file);
        let (title, detail) = large_file_question(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(title, format!("Open “finch-large-q-{}.log”?", std::process::id()));
        assert_eq!(detail, "It’s 300 MB. Opening it may take a while and use a lot of memory.");
        let (title, _) = lossy_question("x.log", "Latin-1", true, None);
        assert!(title.chars().count() <= 45, "{title}");
    }

    #[test]
    fn the_failed_encoding_questions_put_the_encoding_in_the_detail() {
        let (title, detail) = lossy_question("x.txt", "Shift JIS", true, None);
        assert_eq!(title, "“x.txt” won’t save back as it was read");
        assert!(detail.starts_with("It was read as Shift JIS."), "{detail}");
        let (title, detail) = lossy_question("x.txt", "Latin-1", false, Some('é'));
        assert_eq!(title, "Some characters can’t be saved in Latin-1");
        assert_eq!(detail, "The first is “é”. Saving as UTF-8 keeps every character. Cancel leaves the file as it is.");
        let (_, detail) = lossy_question("x.txt", "Latin-1", false, None);
        assert!(detail.starts_with("Saving them as UTF-8"), "{detail}");
    }

    #[test]
    fn special_files_are_refused_without_reading() {
        for path in ["/dev/zero", "/dev/null", "/dev/fd/0"] {
            assert_eq!(read_file(Path::new(path)).err().unwrap().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn large_binary_is_refused_before_the_size_question() {
        let path = std::env::temp_dir().join(format!("finch-sniff-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(OPEN_LIMIT + 1).unwrap();
        drop(file);
        assert_eq!(read_file(&path).err().unwrap().kind(), io::ErrorKind::InvalidData);
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, &vec![b'a'; SNIFF_LEN]).unwrap();
        drop(file);
        assert_eq!(read_file(&path).err().unwrap().kind(), io::ErrorKind::FileTooLarge);
        std::fs::remove_file(path).unwrap();
    }

    fn round_trip(bytes: &[u8]) -> Decoded {
        let decoded = decode(bytes.to_vec()).expect("decodes");
        assert!(!decoded.replaced);
        let (out, lossy) = encode(&decoded.text, decoded.encoding, decoded.has_bom);
        assert!(!lossy);
        assert_eq!(out.as_ref(), bytes);
        decoded
    }

    #[test]
    fn utf8_round_trips_without_guessing() {
        let d = round_trip("héllo wörld\n日本語\n".as_bytes());
        assert_eq!(d.encoding, UTF_8);
        assert!(!d.has_bom);
    }

    #[test]
    fn utf8_bom_is_stripped_and_restored() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello\n");
        let d = round_trip(&bytes);
        assert_eq!(d.text, "hello\n");
        assert!(d.has_bom);
    }

    #[test]
    fn utf16_with_bom_round_trips() {
        for (little, enc) in [(true, UTF_16LE), (false, UTF_16BE)] {
            let (bytes, _) = encode("héllo\n日本", enc, true);
            let d = round_trip(&bytes);
            assert_eq!(d.encoding, enc);
            assert!(d.has_bom);
            assert_eq!(d.text, "héllo\n日本", "little={little}");
        }
    }

    #[test]
    fn utf16_without_bom_is_recognised() {
        let (bytes, _) = encode("fn main() {}\nlet x = 1;\n", UTF_16LE, false);
        let d = round_trip(&bytes);
        assert_eq!(d.encoding, UTF_16LE);
        let (bytes, _) = encode("fn main() {}\nlet x = 1;\n", UTF_16BE, false);
        assert_eq!(round_trip(&bytes).encoding, UTF_16BE);
    }

    #[test]
    fn read_tail_reads_only_what_the_file_gained() {
        let dir = std::env::temp_dir().join(format!("finch-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        let old = "one\ntwo\n";
        std::fs::write(&path, old).unwrap();
        let base = GrowBase { len: old.len(), bom: 0, text: Rope::from(old), full: true, ident: None };
        // As it was (same length): not a growth, the caller reads it whole.
        assert!(read_tail(&path, &base).unwrap().is_none());
        // Grew, and a character is cut short at the end: only the whole characters count.
        let mut grown = old.as_bytes().to_vec();
        grown.extend_from_slice("three \u{e9}".as_bytes());
        grown.pop();
        std::fs::write(&path, &grown).unwrap();
        let tail = read_tail(&path, &base).unwrap().unwrap();
        assert_eq!((tail.text.as_str(), tail.read), ("three ", 7));
        // The old end was changed, the file shrank, or a NUL came in: read it whole.
        std::fs::write(&path, "one\ntwX\nmore\n").unwrap();
        assert!(read_tail(&path, &base).unwrap().is_none());
        std::fs::write(&path, "one\n").unwrap();
        assert!(read_tail(&path, &base).unwrap().is_none());
        std::fs::write(&path, "one\ntwo\nx\0y").unwrap();
        assert!(read_tail(&path, &base).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tail_sees_a_change_far_from_the_end_when_the_file_also_grew() {
        let dir = std::env::temp_dir().join(format!("finch-tail-far-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        let old = format!("[ ] top\n{}", "filler line\n".repeat(2000));
        let base = GrowBase { len: old.len(), bom: 0, text: Rope::from(old.as_str()), full: true, ident: None };
        // Same length at the top, and a line added: the old 4 KB check would have passed.
        std::fs::write(&path, format!("[x] top\n{}new\n", "filler line\n".repeat(2000))).unwrap();
        assert!(read_tail(&path, &base).unwrap().is_none());
        std::fs::write(&path, format!("{old}new\n")).unwrap();
        assert_eq!(read_tail(&path, &base).unwrap().unwrap().text, "new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bounded_growth_check_needs_the_same_file_and_the_same_end() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("finch-tail-bounded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        let old = "filler line\n".repeat(20_000);
        std::fs::write(&path, &old).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let ident = (meta.dev(), meta.ino());
        let base = GrowBase { len: old.len(), bom: 0, text: Rope::from(old.as_str()), full: false, ident: Some(ident) };
        std::fs::write(&path, format!("{old}new\n")).unwrap();
        let tail = read_tail(&path, &base).unwrap().unwrap();
        assert_eq!((tail.text.as_str(), tail.ident), ("new\n", ident));
        // Another file under the name (rotated or replaced) with the same end takes the full read.
        let other = dir.join("other.log");
        std::fs::write(&other, format!("{old}new\n")).unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert!(read_tail(&path, &base).unwrap().is_none());
        // The end of the old text differs.
        std::fs::write(&path, format!("{}X\nnew\n", &old[..old.len() - 2])).unwrap();
        let base = GrowBase { ident: None, full: true, ..base };
        assert!(read_tail(&path, &base).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_known_encoding_that_cannot_write_its_bytes_back_is_lossy() {
        // Shift JIS 0x8790 (NEC row 13) reads as U+2252 and writes back as 0x81E0.
        let bytes = b"abc \x87\x90 def\n";
        let dir = std::env::temp_dir().join(format!("finch-sjis-lossy-reread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sjis.txt");
        std::fs::write(&path, bytes).unwrap();
        let d = reread_file(&path, encoding_rs::SHIFT_JIS).unwrap();
        assert_eq!(d.encoding, encoding_rs::SHIFT_JIS);
        assert!(d.replaced);
        std::fs::write(&path, b"abc \x82\xa0 def\n").unwrap();
        assert!(!reread_file(&path, encoding_rs::SHIFT_JIS).unwrap().replaced);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latin1_round_trips() {
        let (bytes, _, _) = encoding_rs::WINDOWS_1252
            .encode("Café crème brûlée, déjà vu: l'été à Genève était très agréable.\n");
        assert!(std::str::from_utf8(&bytes).is_err());
        let d = round_trip(&bytes);
        assert_eq!(d.encoding, encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn shift_jis_round_trips() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS
            .encode("これは日本語のテキストファイルです。文字コードを保存します。\n");
        let d = round_trip(&bytes);
        assert_eq!(d.encoding, encoding_rs::SHIFT_JIS);
    }

    #[test]
    fn valid_utf8_is_never_guessed_as_another_encoding() {
        for text in ["caf\u{e9}", "\u{e9}\u{e9}\u{e9}", "日本語のテキスト", "ÿþ"] {
            assert_eq!(decode(text.as_bytes().to_vec()).unwrap().encoding, UTF_8);
        }
    }

    #[test]
    fn nul_heavy_binary_is_refused() {
        let mut bytes = vec![0u8; 512];
        bytes.extend_from_slice(&[0xFF, 0x80, 0x00, 0x01, 0xC3]);
        assert_eq!(decode(bytes).err(), Some(DecodeError::Binary));
        assert_eq!(decode(vec![0x7F, b'E', b'L', b'F', 0, 0, 0xFF, 1, 0xFE, 0]).err(), Some(DecodeError::Binary));
    }

    #[test]
    fn unmappable_characters_are_reported() {
        let (_, lossy) = encode("日本", encoding_rs::WINDOWS_1252, false);
        assert!(lossy);
    }

    #[test]
    fn labels_use_the_mac_names() {
        assert_eq!(label(encoding_rs::WINDOWS_1252), "Western (Windows Latin 1)");
        assert_eq!(label(encoding_rs::MACINTOSH), "Western (Mac OS Roman)");
        assert_eq!(label(encoding_rs::SHIFT_JIS), "Japanese (Shift JIS)");
        assert_eq!(label(UTF_16LE), "Unicode (UTF-16 LE)");
        assert_eq!(label(encoding_rs::X_MAC_CYRILLIC), "Cyrillic (Mac OS)");
        assert_eq!(label(encoding_rs::REPLACEMENT), "replacement");
    }

    #[test]
    fn the_first_unfit_character_is_named() {
        assert_eq!(first_unmappable("caf\u{e9} \u{1F600} \u{4e2d}", encoding_rs::WINDOWS_1252), Some('\u{1F600}'));
        assert_eq!(first_unmappable("caf\u{e9}", encoding_rs::WINDOWS_1252), None);
    }

    #[test]
    fn a_reread_keeps_the_encoding_of_text_left_ascii_only() {
        let path = std::env::temp_dir().join(format!("finch-reread-{}.txt", std::process::id()));
        std::fs::write(&path, "caf\n").unwrap();
        assert_eq!(reread_file(&path, encoding_rs::WINDOWS_1252).unwrap().encoding, encoding_rs::WINDOWS_1252);
        assert_eq!(read_file(&path).unwrap().encoding, UTF_8);
        std::fs::write(&path, "caf\u{e9}\n").unwrap();
        assert_eq!(reread_file(&path, encoding_rs::WINDOWS_1252).unwrap().encoding, UTF_8);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn utf16_rewritten_as_ascii_reloads_as_text_and_utf32_is_not_text() {
        let path = std::env::temp_dir().join(format!("finch-reread-utf16-ascii-{}.txt", std::process::id()));
        std::fs::write(&path, "hello world\n").unwrap();
        assert_eq!(reread_file(&path, UTF_16LE).unwrap().text, "hello world\n");
        assert_eq!(reread_file(&path, UTF_16LE).unwrap().encoding, UTF_8);
        std::fs::write(&path, b"\xff\xfe\x00\x00h\x00\x00\x00").unwrap();
        assert!(read_file(&path).is_err());
        assert!(decode(b"\xff\xfe\x00\x00h\x00\x00\x00".to_vec()).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decode_as_takes_the_bytes_the_chosen_way_and_saves_them_back_identical() {
        // "Привет мир" in Windows Cyrillic: the guess reads it as that, a Latin-1 reading is the fix here.
        let bytes = b"\xcf\xf0\xe8\xe2\xe5\xf2 \xec\xe8\xf0, caf\xe9\n".to_vec();
        assert_ne!(decode(bytes.clone()).unwrap().encoding, encoding_rs::WINDOWS_1252, "the guess is the wrong one");
        let decoded = decode_as(&bytes, encoding_rs::WINDOWS_1252);
        assert!(!decoded.replaced);
        assert_eq!(decoded.text, "\u{cf}\u{f0}\u{e8}\u{e2}\u{e5}\u{f2} \u{ec}\u{e8}\u{f0}, caf\u{e9}\n");
        assert_eq!(decoded.encoding, encoding_rs::WINDOWS_1252);
        let (out, lossy) = encode(&decoded.text, decoded.encoding, decoded.has_bom);
        assert!(!lossy);
        assert_eq!(&*out, &bytes[..]);
    }

    #[test]
    fn decode_as_keeps_a_matching_bom_and_flags_bytes_with_no_character() {
        let decoded = decode_as(b"\xef\xbb\xbfhi\n", UTF_8);
        assert!(decoded.has_bom && !decoded.replaced);
        assert_eq!(decoded.text, "hi\n");
        let decoded = decode_as(b"caf\xe9\n", UTF_8);
        assert!(decoded.replaced, "a Latin-1 file is not UTF-8");
        assert!(decoded.text.contains('\u{FFFD}'));
        let decoded = decode_as(b"\xff\xfeh\0i\0", UTF_16LE);
        assert!(decoded.has_bom);
        assert_eq!(decoded.text, "hi");
    }

    #[test]
    fn decode_as_flags_text_that_would_save_back_as_other_bytes() {
        // Shift JIS ED40 (an NEC-selected extension) comes back as FA5C... in the standard encoder.
        let decoded = decode_as(b"a\xed\x40b", encoding_rs::SHIFT_JIS);
        assert!(!decoded.text.contains('\u{FFFD}'));
        assert!(decoded.replaced);
        let decoded = decode_as(b"plain \x93\xfa\x96\x7b\n", encoding_rs::SHIFT_JIS);
        assert!(!decoded.replaced, "ordinary Shift JIS saves back as it was");
    }

    #[test]
    fn odd_utf16_bytes_report_replacement_on_open_and_reread() {
        let bytes = b"\xff\xfeh\0i\0x";
        let decoded = decode(bytes.to_vec()).unwrap();
        assert!(decoded.replaced && decoded.text.contains('\u{FFFD}'));
        let path = std::env::temp_dir().join(format!("finch-odd-utf16-{}.txt", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        assert!(read_file(&path).unwrap().replaced);
        assert!(reread_file(&path, UTF_16LE).unwrap().replaced);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_truncated_multibyte_file_gets_a_lossless_guess() {
        let text = "これは日本語のテキストファイルです。文字コードを保存します。\n".repeat(16);
        // chardetng rejects candidates that need replacement. A truncated Shift JIS pair
        // falls back to Windows Latin 1, which still preserves every original byte.
        let mut bytes = encoding_rs::SHIFT_JIS.encode(&text).0.into_owned();
        bytes.push(0x82);
        let decoded = round_trip(&bytes);
        assert_eq!(decoded.encoding, encoding_rs::WINDOWS_1252);
        assert!(!decoded.replaced);
    }

    #[test]
    fn a_stray_byte_that_round_trips_in_the_guess_does_not_warn() {
        let bytes = b"A mostly ASCII file with one stray byte: caf\xe9.\n";
        let decoded = round_trip(bytes);
        assert_eq!(decoded.encoding, encoding_rs::WINDOWS_1252);
        assert!(!decoded.replaced);
        assert!(!decode("An actual replacement character: \u{FFFD}".as_bytes().to_vec()).unwrap().replaced);
    }

    #[test]
    fn endings_tell_lf_crlf_and_mixed_apart() {
        for (text, want) in [
            ("", Endings::Lf),
            ("one", Endings::Lf),
            ("a\nb\n", Endings::Lf),
            ("a\r\nb\r\n", Endings::Crlf),
            ("a\r\nb\n", Endings::Mixed),
            ("a\nb\r\n", Endings::Mixed),
            ("a\r\nb\rc\r\n", Endings::Lf),
        ] {
            assert_eq!(endings(text), want, "{text:?}");
        }
    }

    #[test]
    fn conversions_rewrite_every_break_and_nothing_else() {
        assert_eq!(to_lf("a\r\nb\nc\r\n\r\nd\re"), "a\nb\nc\n\nd\re");
        assert_eq!(to_crlf("a\r\nb\nc\n\nd\re"), "a\r\nb\r\nc\r\n\r\nd\re");
        assert!(matches!(to_lf("a\nb"), Cow::Borrowed(_)));
        assert!(matches!(to_crlf("a\r\nb"), Cow::Borrowed(_)));
    }
}
