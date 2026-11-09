//! Commands sent to a freshly started helper, before any other command has
//! initialised GStreamer in that process.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn every_take_command_answers_as_the_first_command() {
    let missing = tempfile::TempDir::new().expect("temporary directory");
    let take = missing.path().join("absent.webm");
    let take = take.to_str().expect("UTF-8 path");
    for command in [
        format!(r#"{{"command":"frame_at","path":"{take}","position_ms":0}}"#),
        format!(r#"{{"command":"duration","path":"{take}"}}"#),
    ] {
        let mut helper = Command::new(env!("CARGO_BIN_EXE_kestrel-record"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("helper starts");
        let mut stdin = helper.stdin.take().expect("stdin");
        writeln!(stdin, "{command}").expect("command written");
        drop(stdin);
        let mut reply = String::new();
        BufReader::new(helper.stdout.take().expect("stdout"))
            .read_line(&mut reply)
            .expect("reply read");
        let status = helper.wait().expect("helper exits");
        assert!(status.success(), "{command}: helper exited with {status}");
        assert!(
            reply.contains(r#""reply":"failed""#),
            "{command}: a missing take is reported, got {reply:?}"
        );
    }
}
