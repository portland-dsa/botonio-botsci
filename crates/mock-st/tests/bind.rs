//! The mock fails to start - rather than silently doing nothing - when its port is
//! already taken, and the error names the cause. Binds a loopback port, so run it
//! where loopback TCP is permitted.

#[tokio::test]
async fn spawn_errors_clearly_when_the_port_is_in_use() {
    // Hold a loopback port, then ask the mock to bind the very same address.
    let squatter = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a throwaway port");
    let taken = squatter.local_addr().expect("local addr").to_string();

    let err = mock_st::spawn(&taken, "")
        .await
        .expect_err("spawning on a taken port must fail");

    let msg = err.to_string();
    assert!(
        msg.contains(&taken) && msg.contains("port"),
        "error should name the address and hint at the port: {msg}"
    );
}
