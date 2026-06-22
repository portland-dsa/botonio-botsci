//! Embeds for the scheduled scan.

/// The moderator alert posted when the scan tripwire aborts a pass.
pub fn scan_alert_embed(
    demotions: usize,
    scanned: usize,
    percent: u8,
    floor: usize,
) -> serenity::all::CreateEmbed {
    serenity::all::CreateEmbed::new()
        .title("Scheduled scan paused")
        .description(format!(
            "The scan would have demoted {demotions} of {scanned} members, over the safety \
             threshold ({percent}% and at least {floor}). No roles were changed. This usually \
             means the membership data was incomplete - check Solidarity Tech, then run \
             /refresh-cache before the next scan."
        ))
        .color(0xc8_10_2e)
}
