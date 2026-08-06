use anyhow::{bail, Result};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

pub struct ChunkPos {
    offset: usize,
    tag: [u8; 4],
    compressed_len: usize,
    uncompressed_len: usize,
}

impl ChunkPos {
    // SIGN and END aren't zstd'd at all - compressed_len sits at 0 and the
    // real on-disk payload is uncompressed_len bytes of raw data
    fn on_disk_len(&self) -> usize {
        if self.compressed_len == 0 {
            self.uncompressed_len
        } else {
            self.compressed_len
        }
    }
}

pub fn list_chunks(data: &[u8]) -> Result<Vec<ChunkPos>> {
    if data.len() < 32 || &data[0..8] != b"<roblox!" {
        bail!("not an rbxm file, bad magic");
    }
    let mut pos: usize = 32;
    let mut out: Vec<ChunkPos> = vec![];
    loop {
        if pos + 16 > data.len() {
            bail!("truncated rbxm, ran off the end looking for chunks");
        }
        let tag: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
        let compressed_len: usize = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let uncompressed_len: usize = u32::from_le_bytes(data[pos + 8..pos + 12].try_into().unwrap()) as usize;
        let is_end: bool = &tag == b"END\0";
        let chunk: ChunkPos = ChunkPos { offset: pos, tag, compressed_len, uncompressed_len };
        let on_disk: usize = chunk.on_disk_len();
        out.push(chunk);
        if is_end {
            break;
        }
        pos += 16 + on_disk;
    }
    Ok(out)
}

pub fn decompress_chunk(data: &[u8], chunk: &ChunkPos) -> Result<Vec<u8>> {
    let on_disk: usize = chunk.on_disk_len();
    if on_disk == 0 {
        return Ok(vec![]);
    }
    let payload: &[u8] = &data[chunk.offset + 16..chunk.offset + 16 + on_disk];
    if chunk.compressed_len == 0 {
        return Ok(payload.to_vec());
    }
    if payload.len() < 4 || payload[0..4] != ZSTD_MAGIC {
        bail!(
            "chunk {:?} at 0x{:x} isn't zstd, this rbxm was written by something else",
            String::from_utf8_lossy(&chunk.tag),
            chunk.offset
        );
    }
    let out: Vec<u8> = zstd::stream::decode_all(payload)?;
    if out.len() != chunk.uncompressed_len {
        bail!("decompressed size mismatch for chunk at 0x{:x}", chunk.offset);
    }
    Ok(out)
}

#[derive(Clone)]
pub struct PropChunk {
    pub class_index: i32,
    pub name: String,
    pub type_id: u8,
    pub entries: Vec<Vec<u8>>,
}

// only String(1) and ProtectedString(29) use this length-prefixed layout,
// other property types are fixed-width and need their own parser
pub fn parse_prop_chunk(bytes: &[u8]) -> Result<PropChunk> {
    if bytes.len() < 9 {
        bail!("prop chunk too short to have a header");
    }
    let class_index: i32 = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let name_len: usize = i32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if 8 + name_len >= bytes.len() {
        bail!("prop chunk name length runs off the end");
    }
    let name: String = String::from_utf8_lossy(&bytes[8..8 + name_len]).into_owned();
    let type_id: u8 = bytes[8 + name_len];
    if type_id != 1 && type_id != 29 {
        bail!("prop {name:?} is type {type_id}, not a string type");
    }

    let mut entries: Vec<Vec<u8>> = vec![];
    let mut pos: usize = 8 + name_len + 1;
    while pos + 4 <= bytes.len() {
        let len: usize = i32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + len > bytes.len() {
            bail!("prop {name:?} entry length runs off the end");
        }
        entries.push(bytes[pos..pos + len].to_vec());
        pos += len;
    }
    Ok(PropChunk { class_index, name, type_id, entries })
}

pub fn serialize_prop_chunk(chunk: &PropChunk) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(9 + chunk.name.len());
    out.extend_from_slice(&chunk.class_index.to_le_bytes());
    out.extend_from_slice(&(chunk.name.len() as i32).to_le_bytes());
    out.extend_from_slice(chunk.name.as_bytes());
    out.push(chunk.type_id);
    for entry in &chunk.entries {
        out.extend_from_slice(&(entry.len() as i32).to_le_bytes());
        out.extend_from_slice(entry);
    }
    out
}

fn rebuild_chunk(data: &[u8], chunk: &ChunkPos, new_uncompressed: &[u8]) -> Result<Vec<u8>> {
    let compressed: Vec<u8> = zstd::stream::encode_all(new_uncompressed, 19)?;
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[..chunk.offset]);
    out.extend_from_slice(&chunk.tag);
    out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    out.extend_from_slice(&(new_uncompressed.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&compressed);
    out.extend_from_slice(&data[chunk.offset + 16 + chunk.on_disk_len()..]);
    Ok(out)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w: &[u8]| w == needle)
}

fn find_class_index(data: &[u8], chunks: &[ChunkPos], class_name: &str) -> Result<i32> {
    for chunk in chunks {
        if &chunk.tag != b"INST" {
            continue;
        }
        let decompressed: Vec<u8> = decompress_chunk(data, chunk)?;
        if decompressed.len() < 8 {
            continue;
        }
        let class_index: i32 = i32::from_le_bytes(decompressed[0..4].try_into().unwrap());
        let name_len: usize = i32::from_le_bytes(decompressed[4..8].try_into().unwrap()) as usize;
        if decompressed.len() < 8 + name_len {
            continue;
        }
        if &decompressed[8..8 + name_len] == class_name.as_bytes() {
            return Ok(class_index);
        }
    }
    bail!("no {class_name:?} class in this rbxm")
}

// properties are stored as one array per class, all indexed the same way -
// so the Nth entry in the "Name" array and the Nth entry in "Source" describe
// the same instance. find where Name == module_name and reuse that index.
fn find_entry_index(
    data: &[u8],
    chunks: &[ChunkPos],
    class_index: i32,
    property_name: &str,
    needle: &[u8],
) -> Result<usize> {
    for chunk in chunks {
        if &chunk.tag != b"PROP" {
            continue;
        }
        let decompressed: Vec<u8> = decompress_chunk(data, chunk)?;
        let Ok(prop) = parse_prop_chunk(&decompressed) else {
            continue;
        };
        if prop.class_index != class_index || prop.name != property_name {
            continue;
        }
        if let Some(idx) = prop.entries.iter().position(|e| e.as_slice() == needle) {
            return Ok(idx);
        }
    }
    bail!("no instance named {:?} found in class index {class_index}", String::from_utf8_lossy(needle))
}

// locates a specific named ModuleScript's compiled Source and swaps it for new
// bytecode. offsets shift version to version so nothing here is hardcoded -
// class index comes from INST, instance index comes from matching Name, and
// sanity_markers is a last check that we didn't land on the wrong thing
pub fn patch_module_by_name(
    data: &[u8],
    class_name: &str,
    module_name: &str,
    property_name: &str,
    sanity_markers: &[&str],
    new_bytecode: &[u8],
) -> Result<Vec<u8>> {
    let chunks: Vec<ChunkPos> = list_chunks(data)?;
    let class_index: i32 = find_class_index(data, &chunks, class_name)?;
    let target_idx: usize = find_entry_index(data, &chunks, class_index, "Name", module_name.as_bytes())?;

    for chunk in &chunks {
        if &chunk.tag != b"PROP" {
            continue;
        }
        let decompressed: Vec<u8> = decompress_chunk(data, chunk)?;
        let Ok(prop) = parse_prop_chunk(&decompressed) else {
            continue;
        };
        if prop.class_index != class_index || prop.name != property_name {
            continue;
        }
        let Some(entry) = prop.entries.get(target_idx) else {
            continue;
        };
        if !sanity_markers.iter().all(|m: &&str| contains(entry, m.as_bytes())) {
            bail!(
                "found {module_name:?} but its {property_name:?} doesn't look like what we expect, \
                 roblox may have changed this module - refusing to patch blind"
            );
        }

        let mut patched: PropChunk = prop.clone();
        patched.entries[target_idx] = new_bytecode.to_vec();
        let new_payload: Vec<u8> = serialize_prop_chunk(&patched);
        return rebuild_chunk(data, chunk, &new_payload);
    }
    bail!("no {property_name:?} property chunk for class {class_name:?}")
}
