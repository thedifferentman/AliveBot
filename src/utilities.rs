pub fn napcat_emoji_id(emoji: &emojito::Emoji) -> Option<String> {
    let mut codepoints = emoji
        .codepoint
        .split_ascii_whitespace()
        // FE0F 只是 Emoji 显示样式选择符
        .filter(|cp| !cp.eq_ignore_ascii_case("FE0F"));

    let first = codepoints.next()?;

    // 真正包含多个有效码点，不作为 QQ 回应发送
    if codepoints.next().is_some() {
        return None;
    }

    u32::from_str_radix(first, 16)
        .ok()
        .map(|value| value.to_string())
}