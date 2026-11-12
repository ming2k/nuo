//! DeepSeek effort expansion.

pub(crate) fn deepseek_accepted_efforts(advertised: &[String]) -> Vec<String> {
    /// Documented compatibility aliases: `(alias rung, rung the vendor serves)`.
    const ALIASES: &[(&str, &str)] = &[
        ("minimal", "low"),
        ("medium", "high"),
        ("xhigh", "high"),
        ("ultra", "max"),
    ];
    let mut accepted: Vec<String> = Vec::with_capacity(advertised.len() + ALIASES.len() + 1);
    // `none` is the off switch: accepted whenever the model reasons at all.
    accepted.push("none".to_string());
    accepted.extend(advertised.iter().filter(|level| *level != "none").cloned());
    for (alias, target) in ALIASES {
        if advertised.iter().any(|level| level == target) {
            accepted.push((*alias).to_string());
        }
    }
    accepted.sort_by_key(|level| {
        nuo_provider::Effort::ORDER
            .iter()
            .position(|rung| rung.as_str() == level)
            .unwrap_or(usize::MAX)
    });
    accepted.dedup();
    accepted
}
