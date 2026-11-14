/// The longest tool name the provider APIs take.
pub(super) const MAX_NAME: usize = 64;
/// `mcp__<server>__<tool>`, as a provider API accepts it: letters, digits, `_` and `-` (anything
/// else becomes `_`), at most 64 characters (a longer name is cut and ends in a hash of the whole).
pub fn full_name(server: &str, tool: &str) -> String {
    let raw = format!("mcp__{server}__{tool}");
    let mut name: String =
        raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if name.len() > MAX_NAME {
        let h = raw.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        name.truncate(MAX_NAME - 9);
        name.push_str(&format!("_{:08x}", h as u32));
    }
    name
}
