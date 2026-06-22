//! The dispatch decision for a button press that arrives while a modal is open.
//!
//! A modal dismissed without submitting fires no Discord event, so the buttons on the
//! host message stay live. When one is pressed during the wait, this decides whether the
//! press reopens the same modal - the moderator re-clicked the button that opened it - or
//! should be handed back to the wizard's main dispatch (Skip, Stop, Override, or a fresh
//! lookup). Kept free of `serenity` so it can be exercised offline.

/// What a button press means while the modal opened by some trigger button is still live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReclickAction {
    /// Reopen the same modal in place: the moderator pressed the button that opened it.
    Reopen,
    /// Hand the press back to the outer dispatch: a different control was pressed, so the
    /// modal stays closed and the press is routed (Skip, Stop, Override, a fresh lookup).
    Redispatch,
}

/// Decide what a press of `pressed` means while the modal opened by `trigger` is live.
///
/// Only a re-click of the trigger reopens the modal; every other button is redispatched.
/// Treating any press as a re-click was the bug that let Skip/Stop reopen the email modal
/// instead of advancing the bulk wizard.
pub(crate) fn reclick_action(pressed: &str, trigger: &str) -> ReclickAction {
    if pressed == trigger {
        ReclickAction::Reopen
    } else {
        ReclickAction::Redispatch
    }
}
