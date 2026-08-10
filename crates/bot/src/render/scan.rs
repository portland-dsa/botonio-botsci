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

/// The verification-log summary posted when a pass held members inside the temporary
/// payment-processing window.
///
/// Posted once per pass and only when `held` is non-zero, so the channel falls silent as
/// soon as the upstream defect stops producing these - which is the signal to remove the
/// `lapse-grace` feature.
#[cfg(feature = "lapse-grace")]
pub fn lapse_grace_embed(held: usize, scanned: usize) -> serenity::all::CreateEmbed {
    let members = if held == 1 { "member" } else { "members" };
    serenity::all::CreateEmbed::new()
        .title("Dues holds this scan")
        .description(format!(
            "{held} of {scanned} {members} are recorded as lapsed but have an expiry date \
             recent enough that their renewal payment is most likely still processing. They \
             were held at Member rather than demoted.\n\nThis works around a known upstream \
             defect. When this message stops appearing, the defect is fixed and the \
             workaround can be removed."
        ))
        .color(0xfa_a6_1a)
}
