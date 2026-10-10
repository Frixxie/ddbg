use std::path::Path;

/// 1-based line of the `ddbg:<marker>` comment in a fixture source file.
pub fn line_of(path: impl AsRef<Path>, marker: &str) -> u32 {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let tag = format!("ddbg:{marker}");
    let hits: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            l.find(&tag).is_some_and(|i| {
                !l[i + tag.len()..]
                    .starts_with(|c: char| c.is_alphanumeric() || c == '-' || c == '_')
            })
        })
        .map(|(i, _)| i + 1)
        .collect();
    match hits[..] {
        [line] => line as u32,
        [] => panic!("marker `{tag}` not found in {}", path.display()),
        _ => panic!(
            "marker `{tag}` found more than once in {}: lines {hits:?}",
            path.display()
        ),
    }
}
