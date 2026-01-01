use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use byteorder::{BigEndian, ReadBytesExt};
use std::fs::File;
use std::collections::HashMap;

/// RPM file format constants
const RPM_MAGIC: u32 = 0xedabeedb;
const RPM_HEADER_MAGIC: u32 = 0x8eade801;

/// RPM header tag definitions (subset we care about)
const RPMTAG_NAME: u32 = 1000;
const RPMTAG_VERSION: u32 = 1001;
const RPMTAG_RELEASE: u32 = 1002;
const RPMTAG_SUMMARY: u32 = 1004;
const RPMTAG_DESCRIPTION: u32 = 1005;
const RPMTAG_SIZE: u32 = 1009;
const RPMTAG_REQUIRENAME: u32 = 1049;
const RPMTAG_REQUIREVERSION: u32 = 1050;
const RPMTAG_CONFLICTNAME: u32 = 1054;
const RPMTAG_PROVIDESNAME: u32 = 1047;
const RPMTAG_OBSOLETESNAME: u32 = 1090;
const RPMTAG_SUGGESTSNAME: u32 = 5048;
const RPMTAG_RECOMMENDSNAME: u32 = 5046;
const RPMTAG_SUPPLEMENTSNAME: u32 = 5050;
const RPMTAG_ENHANCESNAME: u32 = 5052;

/// RPM lead structure (96 bytes)
#[derive(Debug)]
struct RPMLoad {
    magic: u32,
    major: u8,
    minor: u8,
    rpm_type: u16,
    archnum: u16,
    name: [u8; 66],
    osnum: u16,
    signature_type: u16,
    reserved: [u8; 16],
}

/// RPM header structure
#[derive(Debug)]
struct RPMHeader {
    magic: u32,
    reserved: [u8; 4],
    nindex: u32,
    hsize: u32,
}

/// RPM index entry
#[derive(Debug)]
struct RPMIndexEntry {
    tag: u32,
    rpm_type: u32,
    offset: u32,
    count: u32,
}

/// Parsed RPM package information
#[derive(Debug, Clone)]
pub struct RPMInfo {
    pub name: String,
    pub version: String,
    pub release: String,
    pub summary: String,
    pub description: String,
    pub size: u64,
    pub dependencies: Vec<String>,
    pub provides: Vec<String>,
}

/// Parse an RPM file natively without external commands
pub fn parse_rpm_file(path: &Path) -> Result<RPMInfo, String> {
    let mut file = File::open(path)
        .map_err(|e| format!("Failed to open RPM file: {}", e))?;

    // Read and validate RPM lead
    let lead = read_rpm_lead(&mut file)?;
    if lead.magic != RPM_MAGIC {
        return Err("Invalid RPM magic number".to_string());
    }

    // Skip signature (we don't validate signatures for now)
    skip_rpm_signature(&mut file)?;

    // Read header
    let header = read_rpm_header(&mut file)?;

    // Read index entries
    let index_entries = read_rpm_index(&mut file, header.nindex)?;

    // Read header data store
    let header_data = read_header_data(&mut file, header.hsize)?;

    // Parse index entries to extract metadata
    let metadata = parse_rpm_metadata(&index_entries, &header_data)?;

    Ok(metadata)
}

/// Read RPM lead structure
fn read_rpm_lead<R: Read>(reader: &mut R) -> Result<RPMLoad, String> {
    let magic = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read RPM magic: {}", e))?;

    let major = reader.read_u8()
        .map_err(|e| format!("Failed to read major version: {}", e))?;

    let minor = reader.read_u8()
        .map_err(|e| format!("Failed to read minor version: {}", e))?;

    let rpm_type = reader.read_u16::<BigEndian>()
        .map_err(|e| format!("Failed to read RPM type: {}", e))?;

    let archnum = reader.read_u16::<BigEndian>()
        .map_err(|e| format!("Failed to read arch number: {}", e))?;

    let mut name = [0u8; 66];
    reader.read_exact(&mut name)
        .map_err(|e| format!("Failed to read name: {}", e))?;

    let osnum = reader.read_u16::<BigEndian>()
        .map_err(|e| format!("Failed to read OS number: {}", e))?;

    let signature_type = reader.read_u16::<BigEndian>()
        .map_err(|e| format!("Failed to read signature type: {}", e))?;

    let mut reserved = [0u8; 16];
    reader.read_exact(&mut reserved)
        .map_err(|e| format!("Failed to read reserved: {}", e))?;

    Ok(RPMLoad {
        magic,
        major,
        minor,
        rpm_type,
        archnum,
        name,
        osnum,
        signature_type,
        reserved,
    })
}

/// Skip RPM signature (simplified - we don't validate signatures)
fn skip_rpm_signature<R: Read + Seek>(reader: &mut R) -> Result<(), String> {
    // Read signature header
    let sig_magic = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read signature magic: {}", e))?;

    if sig_magic != RPM_HEADER_MAGIC {
        return Err("Invalid signature header magic".to_string());
    }

    // Skip reserved
    reader.seek(SeekFrom::Current(4))
        .map_err(|e| format!("Failed to skip signature reserved: {}", e))?;

    let nindex = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read signature nindex: {}", e))?;

    let hsize = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read signature hsize: {}", e))?;

    // Skip index entries and data
    let skip_size = (nindex * 16) + hsize;
    reader.seek(SeekFrom::Current(skip_size as i64))
        .map_err(|e| format!("Failed to skip signature: {}", e))?;

    Ok(())
}

/// Read RPM header
fn read_rpm_header<R: Read>(reader: &mut R) -> Result<RPMHeader, String> {
    let magic = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read header magic: {}", e))?;

    if magic != RPM_HEADER_MAGIC {
        return Err("Invalid header magic".to_string());
    }

    let mut reserved = [0u8; 4];
    reader.read_exact(&mut reserved)
        .map_err(|e| format!("Failed to read header reserved: {}", e))?;

    let nindex = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read header nindex: {}", e))?;

    let hsize = reader.read_u32::<BigEndian>()
        .map_err(|e| format!("Failed to read header hsize: {}", e))?;

    Ok(RPMHeader {
        magic,
        reserved,
        nindex,
        hsize,
    })
}

/// Read RPM index entries
fn read_rpm_index<R: Read>(reader: &mut R, nindex: u32) -> Result<Vec<RPMIndexEntry>, String> {
    let mut entries = Vec::new();

    for _ in 0..nindex {
        let tag = reader.read_u32::<BigEndian>()
            .map_err(|e| format!("Failed to read index tag: {}", e))?;

        let rpm_type = reader.read_u32::<BigEndian>()
            .map_err(|e| format!("Failed to read index type: {}", e))?;

        let offset = reader.read_u32::<BigEndian>()
            .map_err(|e| format!("Failed to read index offset: {}", e))?;

        let count = reader.read_u32::<BigEndian>()
            .map_err(|e| format!("Failed to read index count: {}", e))?;

        entries.push(RPMIndexEntry {
            tag,
            rpm_type,
            offset,
            count,
        });
    }

    Ok(entries)
}

/// Read header data store
fn read_header_data<R: Read>(reader: &mut R, hsize: u32) -> Result<Vec<u8>, String> {
    let mut data = vec![0u8; hsize as usize];
    reader.read_exact(&mut data)
        .map_err(|e| format!("Failed to read header data: {}", e))?;
    Ok(data)
}

/// Parse RPM metadata from index entries and data
fn parse_rpm_metadata(index_entries: &[RPMIndexEntry], data: &[u8]) -> Result<RPMInfo, String> {
    let mut name = None;
    let mut version = None;
    let mut release = None;
    let mut summary = None;
    let mut description = None;
    let mut size = 0u64;
    let mut dependencies = Vec::new();
    let mut provides = Vec::new();

    for entry in index_entries {
        match entry.tag {
            RPMTAG_NAME => {
                if let Some(value) = extract_string_value(data, entry) {
                    name = Some(value);
                }
            }
            RPMTAG_VERSION => {
                if let Some(value) = extract_string_value(data, entry) {
                    version = Some(value);
                }
            }
            RPMTAG_RELEASE => {
                if let Some(value) = extract_string_value(data, entry) {
                    release = Some(value);
                }
            }
            RPMTAG_SUMMARY => {
                if let Some(value) = extract_string_value(data, entry) {
                    summary = Some(value);
                }
            }
            RPMTAG_DESCRIPTION => {
                if let Some(value) = extract_string_value(data, entry) {
                    description = Some(value);
                }
            }
            RPMTAG_SIZE => {
                if let Some(value) = extract_u64_value(data, entry) {
                    size = value;
                }
            }
            RPMTAG_REQUIRENAME => {
                if let Some(deps) = extract_string_array_value(data, entry) {
                    dependencies.extend(deps);
                }
            }
            RPMTAG_PROVIDESNAME => {
                if let Some(provs) = extract_string_array_value(data, entry) {
                    provides.extend(provs);
                }
            }
            _ => {} // Ignore other tags
        }
    }

    Ok(RPMInfo {
        name: name.unwrap_or_default(),
        version: version.unwrap_or_default(),
        release: release.unwrap_or_default(),
        summary: summary.unwrap_or_default(),
        description: description.unwrap_or_default(),
        size,
        dependencies,
        provides,
    })
}

/// Extract string value from header data
fn extract_string_value(data: &[u8], entry: &RPMIndexEntry) -> Option<String> {
    if entry.offset as usize + 1 > data.len() {
        return None;
    }

    // Find null terminator
    let start = entry.offset as usize;
    let end = data[start..].iter().position(|&b| b == 0).map(|pos| start + pos)?;

    String::from_utf8(data[start..end].to_vec()).ok()
}

/// Extract u64 value from header data
fn extract_u64_value(data: &[u8], entry: &RPMIndexEntry) -> Option<u64> {
    if entry.offset as usize + 8 > data.len() {
        return None;
    }

    let mut buf = &data[entry.offset as usize..entry.offset as usize + 8];
    buf.read_u64::<BigEndian>().ok()
}

/// Extract string array value from header data
fn extract_string_array_value(data: &[u8], entry: &RPMIndexEntry) -> Option<Vec<String>> {
    let mut strings = Vec::new();

    for i in 0..entry.count {
        let offset = entry.offset as usize + (i as usize * 4); // String offsets are 4 bytes each
        if offset + 4 > data.len() {
            break;
        }

        let string_offset = {
            let mut buf = &data[offset..offset + 4];
            buf.read_u32::<BigEndian>().ok()?
        } as usize;

        if string_offset >= data.len() {
            continue;
        }

        // Find null terminator
        let end = data[string_offset..].iter().position(|&b| b == 0)
            .map(|pos| string_offset + pos)
            .unwrap_or(data.len());

        if let Ok(s) = String::from_utf8(data[string_offset..end].to_vec()) {
            if !s.is_empty() {
                strings.push(s);
            }
        }
    }

    Some(strings)
}

/// Extract RPM payload (cpio archive) to a directory
pub fn extract_rpm_payload(rpm_path: &Path, extract_dir: &Path) -> Result<(), String> {
    let mut file = File::open(rpm_path)
        .map_err(|e| format!("Failed to open RPM file: {}", e))?;

    // Skip lead
    file.seek(SeekFrom::Start(96))
        .map_err(|e| format!("Failed to skip RPM lead: {}", e))?;

    // Skip signature
    skip_rpm_signature(&mut file)?;

    // Skip header
    let header = read_rpm_header(&mut file)?;
    file.seek(SeekFrom::Current((header.nindex * 16 + header.hsize) as i64))
        .map_err(|e| format!("Failed to skip header: {}", e))?;

    // The rest is the cpio payload - extract it
    extract_cpio_archive(&mut file, extract_dir)
}

/// Extract cpio archive (simplified implementation)
fn extract_cpio_archive<R: Read>(reader: &mut R, extract_dir: &Path) -> Result<(), String> {
    // This is a very basic cpio extractor - in a real implementation,
    // we'd need proper cpio format parsing
    // For now, we'll use the existing cpio command as a fallback

    use std::process::Command;
    use std::io::Write;

    // Create a temporary file for the cpio data
    let mut temp_file = tempfile::NamedTempFile::new()
        .map_err(|e| format!("Failed to create temp file: {}", e))?;

    // Copy reader to temp file (this is inefficient but works)
    std::io::copy(reader, &mut temp_file)
        .map_err(|e| format!("Failed to copy cpio data: {}", e))?;

    let temp_path = temp_file.path().to_path_buf();
    temp_file.keep()
        .map_err(|e| format!("Failed to keep temp file: {}", e))?;

    // Extract using cpio command with --no-absolute-filenames to prevent absolute path extraction
    let status = Command::new("cpio")
        .arg("-idmv")
        .arg("--no-absolute-filenames")
        .current_dir(extract_dir)
        .stdin(std::fs::File::open(&temp_path)
            .map_err(|e| format!("Failed to reopen temp file: {}", e))?)
        .status()
        .map_err(|e| format!("Failed to run cpio: {}", e))?;

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_path);

    if status.success() {
        Ok(())
    } else {
        Err("cpio extraction failed".to_string())
    }
}
