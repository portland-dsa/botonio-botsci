//! Drains a backend's paginated member read into one list, driving a progress
//! bar as it goes.
//!
//! The backends expose pagination as a concrete `members_page(cursor) ->
//! MemberPage` cursor and know nothing about progress (see
//! [`backends::MemberPage`] for why it's a manual cursor rather than a
//! `Stream`). [`drain_pages`] is the consumer side: it owns the page loop and the
//! [`Progress`] bar, so the front end's progress concern lives here, fully
//! generic - no `dyn`.

use std::future::Future;

use crate::backends::MemberPage;
use crate::seam::{Progress, ProgressBar};

/// Repeatedly calls `fetch(cursor)` until a page reports no `next`, collecting
/// every member and advancing a progress bar.
///
/// The bar is created from the first page: a determinate bar when the backend
/// reports a [`total`](MemberPage::total) (Solidarity Tech does), otherwise a
/// spinner whose message tracks the running "found" count. Position advances by
/// each page's [`scanned`](MemberPage::scanned) count, so it reflects rows
/// *processed* even when a backend filters some of them out.
///
/// Generic over the fetch closure and the [`Progress`] impl, so there is no
/// `dyn` anywhere; the backend error `E` propagates unchanged.
pub async fn drain_pages<T, E, P, Fetch, Fut>(
    progress: &P,
    label: &str,
    mut fetch: Fetch,
) -> Result<Vec<T>, E>
where
    P: Progress,
    Fetch: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<MemberPage<T>, E>>,
{
    let mut out: Vec<T> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut bar: Option<P::Bar> = None;
    let mut spinner = false;
    let mut found: u64 = 0;

    loop {
        let page = fetch(cursor.take()).await?;

        if bar.is_none() {
            bar = Some(match page.total {
                Some(total) => progress.bar(total, label),
                None => {
                    spinner = true;
                    progress.spinner(label)
                }
            });
        }

        found += page.members.len() as u64;
        let scanned = page.scanned;
        let next = page.next;
        out.extend(page.members);

        if let Some(b) = &bar {
            b.inc(scanned);
            // The determinate bar already shows position/total; only the spinner
            // needs the secondary found-count surfaced in its message.
            if spinner {
                b.set_message(&format!("{label} · {found} found"));
            }
        }

        match next {
            Some(n) => cursor = Some(n),
            None => break,
        }
    }

    if let Some(b) = bar {
        b.finish_and_clear();
    }
    Ok(out)
}
