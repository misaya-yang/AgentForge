use super::ContentError;
pub fn xml_reference(name: &str) -> Result<String, ContentError> {
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        s if s.starts_with("#x") => u32::from_str_radix(&s[2..], 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| ContentError::new(422, "Invalid XML reference"))?,
        s if s.starts_with('#') => s[1..]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| ContentError::new(422, "Invalid XML reference"))?,
        _ => return Err(ContentError::new(422, "Invalid XML reference")),
    };
    Ok(c.to_string())
}
