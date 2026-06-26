//! One-shot SSO signing-keypair generator. Run once per environment at provisioning:
//! `cargo run -p discord-bot --example sso_keygen`. SOPS-encrypt the printed secret
//! (it becomes the `sso_signing_key` credential); hand the public half to
//! workspace-sync, which pins it by `kid`. The secret never touches CI or a repo.

use pasetors::keys::{AsymmetricKeyPair, Generate};
use pasetors::version4::V4;

fn main() {
    let kp = AsymmetricKeyPair::<V4>::generate().expect("keygen");
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    println!(
        "secret_hex (SOPS-encrypt as sso_signing_key): {}",
        hex(kp.secret.as_bytes())
    );
    println!(
        "public_hex (give to workspace-sync):          {}",
        hex(kp.public.as_bytes())
    );
}
