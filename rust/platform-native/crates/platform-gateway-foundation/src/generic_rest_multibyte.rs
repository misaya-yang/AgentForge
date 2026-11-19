//! Immutable incremental multibyte codec transducers captured from the source
//! Python codec library with errors=replace. A transition stores next state,
//! up to three emitted Unicode scalars, and a final-flush string per state.
use serde_json::Value;
use std::sync::LazyLock;
static MANIFEST: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("generic_rest_multibyte/manifest.json"))
        .expect("embedded multibyte codec manifest")
});
fn bytes(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "big5" => include_bytes!("generic_rest_multibyte/big5.bin"),
        "big5hkscs" => include_bytes!("generic_rest_multibyte/big5hkscs.bin"),
        "cp932" => include_bytes!("generic_rest_multibyte/cp932.bin"),
        "cp949" => include_bytes!("generic_rest_multibyte/cp949.bin"),
        "cp950" => include_bytes!("generic_rest_multibyte/cp950.bin"),
        "euc_jp" => include_bytes!("generic_rest_multibyte/euc_jp.bin"),
        "euc_jis_2004" => include_bytes!("generic_rest_multibyte/euc_jis_2004.bin"),
        "euc_jisx0213" => include_bytes!("generic_rest_multibyte/euc_jisx0213.bin"),
        "gb2312" => include_bytes!("generic_rest_multibyte/gb2312.bin"),
        "gbk" => include_bytes!("generic_rest_multibyte/gbk.bin"),
        "johab" => include_bytes!("generic_rest_multibyte/johab.bin"),
        "shift_jis" => include_bytes!("generic_rest_multibyte/shift_jis.bin"),
        "shift_jis_2004" => include_bytes!("generic_rest_multibyte/shift_jis_2004.bin"),
        "shift_jisx0213" => include_bytes!("generic_rest_multibyte/shift_jisx0213.bin"),
        _ => return None,
    })
}
pub fn decode(label: &str, input: &[u8]) -> Option<String> {
    let (name, config) = MANIFEST.as_object()?.iter().find(|(_, v)| {
        v["aliases"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(label)))
    })?;
    let table = bytes(name)?;
    let mut state = 0usize;
    let mut output = String::new();
    for byte in input {
        let offset = (state * 256 + usize::from(*byte)) * 12;
        let record = table.get(offset..offset + 12)?;
        let word = u64::from_le_bytes(record[..8].try_into().ok()?);
        state = (word & 0xffff) as usize;
        let length = (word >> 58) & 3;
        for index in 0..length {
            let scalar = if index < 2 {
                ((word >> (16 + index * 21)) & 0x1fffff) as u32
            } else {
                u32::from_le_bytes(record[8..12].try_into().ok()?)
            };
            output.push(char::from_u32(scalar)?);
        }
    }
    output.push_str(config["final"][state].as_str()?);
    Some(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_embedded_transducers_have_complete_transition_tables() {
        for (name, v) in MANIFEST.as_object().unwrap() {
            let states = v["states"].as_u64().unwrap() as usize;
            let table = bytes(name).unwrap();
            assert_eq!(table.len(), states * 256 * 12);
            assert_eq!(v["final"].as_array().unwrap().len(), states);
            for record in table.chunks_exact(12) {
                let word = u64::from_le_bytes(record[..8].try_into().unwrap());
                assert!(((word & 0xffff) as usize) < states);
            }
        }
    }
}
