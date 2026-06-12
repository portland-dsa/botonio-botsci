//! Per-scenario harness: the `EspioWorld` constructor, its members accessor, and
//! the `Debug` impl `cucumber::World` requires.

use std::fmt;

use secrecy::SecretString;
use wiremock::MockServer;

use backends::solidarity_tech::{SolidarityTechError, SolidarityTechHttp, SolidarityTechMember};

use crate::{EspioWorld, Outcome, TOKEN};

impl EspioWorld {
    pub(crate) async fn new() -> Self {
        let server = MockServer::start().await;
        let client =
            SolidarityTechHttp::with_base_url(server.uri(), SecretString::from(TOKEN.to_string()));
        Self {
            server,
            client,
            last: None,
            elapsed: None,
        }
    }

    pub(crate) fn members(&self) -> &Result<Vec<SolidarityTechMember>, SolidarityTechError> {
        match self.last.as_ref().expect("no call was made") {
            Outcome::Members(r) => r,
            _ => panic!("last call did not return members"),
        }
    }
}

// `cucumber::World` requires `Debug`, but neither the `MockServer` nor the
// client is `Debug`; print only what is inspectable.
impl fmt::Debug for EspioWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspioWorld")
            .field("uri", &self.server.uri())
            .field("has_outcome", &self.last.is_some())
            .finish_non_exhaustive()
    }
}
