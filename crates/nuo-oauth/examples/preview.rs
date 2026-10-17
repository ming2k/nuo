use std::fs;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let success_html = nuo_oauth::render_page(
        "GitHub",
        "Authorization successful! You may now close this tab and return to the terminal.",
        true,
    );
    let failure_html = nuo_oauth::render_page(
        "Google",
        "access_denied: The user canceled the authorization request.",
        false,
    );

    // Save static HTML preview files to target directory
    let target_dir = PathBuf::from("target");
    let _ = fs::create_dir_all(&target_dir);

    let success_path = target_dir.join("oauth_preview_success.html");
    let failure_path = target_dir.join("oauth_preview_failure.html");

    fs::write(&success_path, &success_html)?;
    fs::write(&failure_path, &failure_html)?;

    let abs_success = fs::canonicalize(&success_path).unwrap_or(success_path);
    let abs_failure = fs::canonicalize(&failure_path).unwrap_or(failure_path);

    println!("\n╔═══════════════════════════════════════════════════════════════════════╗");
    println!("║                 NUO OAuth2 Preview Files Generated                    ║");
    println!("╠═══════════════════════════════════════════════════════════════════════╣");
    println!("║ [1] Success (200 OK):                                                 ║");
    println!("║     file://{} ║", abs_success.display());
    println!("║ [2] Failure (400 ERR):                                                ║");
    println!("║     file://{} ║", abs_failure.display());
    println!("╚═══════════════════════════════════════════════════════════════════════╝\n");

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 40888)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    let port = listener.local_addr()?.port();

    println!("🌐 Local Live Preview Server running at:");
    println!("   ▶ Success view: http://127.0.0.1:{port}/");
    println!("   ▶ Failure view: http://127.0.0.1:{port}/error");
    println!("\n(The preview files were also written to disk above)\n");

    // Also support non-interactive / finite run when NUO_PREVIEW_ONCE=1
    if std::env::var("NUO_PREVIEW_ONCE").is_ok() {
        println!("NUO_PREVIEW_ONCE set: exiting cleanly after writing HTML files.");
        return Ok(());
    }

    println!("Press Ctrl+C to stop the preview server.\n");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let success = success_html.clone();
        let failure = failure_html.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");

            let (body, status) = if path == "/error" {
                (failure, "400 Bad Request")
            } else {
                (success, "200 OK")
            };

            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.flush().await;
        });
    }
}
