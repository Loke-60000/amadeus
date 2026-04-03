use std::{
    borrow::Cow,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{
    WebViewBuilder,
    http::{
        Request, Response, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
};

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
)))]
use wry::WebViewBuilderExtUnix;

use crate::core::error::{AppError, AppResult};

const LOG_WINDOW_TITLE: &str = "Amadeus-logs";
const LOG_PROTOCOL_NAME: &str = "amadeus-log";
const MAX_LOG_BYTES: u64 = 200_000;
const LOG_WINDOW_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Amadeus-logs</title>
    <style>
      :root {
        color-scheme: dark;
        --bg: #071018;
        --panel: #0d1722;
        --border: #1b3147;
        --text: #d9f7ff;
        --muted: #7fa2b8;
        --accent: #61f3c3;
        --shadow: rgba(0, 0, 0, 0.3);
      }

      * {
        box-sizing: border-box;
      }

      body {
        margin: 0;
        min-height: 100vh;
        background:
          radial-gradient(circle at top, rgba(97, 243, 195, 0.12), transparent 35%),
          linear-gradient(180deg, #071018 0%, #03070c 100%);
        color: var(--text);
        font-family: "JetBrains Mono", "Fira Code", "IBM Plex Mono", monospace;
      }

      .shell {
        display: grid;
        grid-template-rows: auto 1fr auto;
        height: 100vh;
        padding: 14px;
        gap: 12px;
      }

      .bar {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 16px;
        padding: 12px 14px;
        border: 1px solid var(--border);
        border-radius: 14px;
        background: rgba(13, 23, 34, 0.88);
        box-shadow: 0 14px 32px var(--shadow);
      }

      .title {
        font-size: 13px;
        font-weight: 700;
        letter-spacing: 0.08em;
        text-transform: uppercase;
        color: var(--accent);
      }

      .status {
        font-size: 12px;
        color: var(--muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
      }

      pre {
        margin: 0;
        padding: 18px;
        border: 1px solid var(--border);
        border-radius: 16px;
        background: rgba(5, 11, 17, 0.94);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.03), 0 18px 40px var(--shadow);
        overflow: auto;
        white-space: pre-wrap;
        word-break: break-word;
        line-height: 1.45;
        font-size: 13px;
      }

      .footer {
        font-size: 11px;
        color: var(--muted);
        opacity: 0.9;
        padding: 0 4px;
      }
    </style>
  </head>
  <body>
    <div class="shell">
      <div class="bar">
        <div class="title">Amadeus-logs</div>
        <div class="status" id="status">Connecting…</div>
      </div>
      <pre id="log">Waiting for chat and native logs…</pre>
      <div class="footer">Live tail of the chat transcript and native Cubism runtime log.</div>
    </div>
    <script>
      const logPath = __LOG_PATH__;
      const statusNode = document.getElementById("status");
      const logNode = document.getElementById("log");
      let lastText = null;

      function shouldStickToBottom() {
        return logNode.scrollTop + logNode.clientHeight >= logNode.scrollHeight - 24;
      }

      async function refresh() {
        try {
          const stick = shouldStickToBottom();
          const response = await fetch("amadeus-log://localhost/tail", { cache: "no-store" });
          const text = await response.text();
          statusNode.textContent = `Streaming ${logPath}`;

          if (text !== lastText) {
            lastText = text;
            logNode.textContent = text || "Waiting for chat and native logs…";
            if (stick) {
              requestAnimationFrame(() => {
                logNode.scrollTop = logNode.scrollHeight;
              });
            }
          }
        } catch (error) {
          statusNode.textContent = `Log viewer error: ${error}`;
        }
      }

      refresh();
      setInterval(refresh, 250);
    </script>
  </body>
</html>
"#;

pub fn run_log_viewer(log_path: PathBuf) -> AppResult<()> {
    let log_path = Arc::new(log_path);
    let event_loop = EventLoopBuilder::<()>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(LOG_WINDOW_TITLE)
        .with_inner_size(LogicalSize::new(980.0, 720.0))
        .with_min_inner_size(LogicalSize::new(680.0, 420.0))
        .build(&event_loop)?;

    let protocol_log_path = Arc::clone(&log_path);
    let builder = WebViewBuilder::new()
        .with_asynchronous_custom_protocol(
            LOG_PROTOCOL_NAME.into(),
            move |_webview_id, request, responder| {
                responder.respond(handle_log_request(&protocol_log_path, request));
            },
        )
      .with_url("amadeus-log://localhost/index.html");

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let _webview = builder.build(&window)?;

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;

        let vbox = window.default_vbox().ok_or(AppError::MissingGtkContainer)?;
        builder.build_gtk(vbox)?
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });

    #[allow(unreachable_code)]
    Ok(())
}

fn handle_log_request(
    log_path: &Path,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    match request.uri().path() {
        "/" | "/index.html" => html_response(render_log_viewer_html(log_path)),
        "/tail" => text_response(read_log_tail(log_path)),
        _ => error_response(StatusCode::NOT_FOUND, "Unknown Amadeus-logs resource"),
    }
}

fn render_log_viewer_html(log_path: &Path) -> String {
    let path_literal = serde_json::to_string(&log_path.display().to_string())
        .unwrap_or_else(|_| "\"native log\"".to_string());
    LOG_WINDOW_HTML.replace("__LOG_PATH__", &path_literal)
}

fn read_log_tail(log_path: &Path) -> String {
    let mut file = match File::open(log_path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };

    let file_len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = file_len.saturating_sub(MAX_LOG_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }

    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }

    if start > 0 {
        if let Some(newline_index) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline_index);
        }
    }

    String::from_utf8_lossy(&bytes).into_owned()
}

fn html_response(html: String) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Cow::Owned(html.into_bytes()))
        .expect("valid Amadeus-logs HTML response")
}

fn text_response(body: String) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Cow::Owned(body.into_bytes()))
        .expect("valid Amadeus-logs text response")
}

fn error_response(status: StatusCode, message: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Cow::Owned(message.as_bytes().to_vec()))
        .expect("valid Amadeus-logs error response")
}
