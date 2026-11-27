use std::{net::SocketAddr, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::Command,
};

#[tokio::test]
async fn serves_on_loopback_and_prints_local_url() {
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        data.path().join("config.json"),
        r#"{"agents":{"claude":{"enabled":false},"codex":{"enabled":false},"antigravity":{"enabled":false},"devin":{"enabled":false},"kiro":{"enabled":false}}}"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_verre"))
        .args(["serve", "--port", "0"])
        .env("VERRE_HOME", data.path())
        .env_remove("VERRE_WEB_DIR")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut url = String::new();
        stdout.read_line(&mut url).await.unwrap();
        let (address, token) = url
            .trim()
            .strip_prefix("http://")
            .and_then(|url| url.split_once("/#token="))
            .expect("expected a local authentication URL");
        let socket_address: SocketAddr = address.parse().unwrap();
        assert_eq!(socket_address.ip().to_string(), "127.0.0.1");
        assert_eq!(token.len(), 64);
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data.path().join("state.json")).unwrap())
                .unwrap();
        assert!(state["localToken"].as_str() == Some(token));

        let mut stream = TcpStream::connect(socket_address).await.unwrap();
        stream
            .write_all(
                format!("GET /api/health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 "));
        assert!(reply.contains("\"ok\":true"));

        let body = serde_json::json!({"token": token}).to_string();
        let mut stream = TcpStream::connect(socket_address).await.unwrap();
        stream.write_all(format!(
            "POST /api/auth/local HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ).as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 "));
        assert!(!reply.to_ascii_lowercase().contains("\r\nset-cookie:"));
        let body: serde_json::Value = serde_json::from_str(reply.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["remoteClient"], false);
        let session = body["session"].as_str().unwrap();
        let mut stream = TcpStream::connect(socket_address).await.unwrap();
        stream.write_all(format!(
            "GET /api/auth/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nAuthorization: Bearer {session}\r\n\r\n"
        ).as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 "));
        assert!(reply.contains("\"authenticated\":true"));
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
}
