use std::io::{self, ErrorKind, Read};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::{env, fs, process};
use wire::{bridge_error, connect_to_bridge, send_line, socket_address};

fn unique_name(suffix: &str) -> String {
    format!("kid-agentic-coding-wire-{suffix}-{}", process::id())
}

fn bind_abstract(name: &str) -> UnixListener {
    let address = SocketAddr::from_abstract_name(name.as_bytes()).expect("address is valid");
    UnixListener::bind_addr(&address).expect("bind succeeds")
}

#[test]
fn socket_address_distinguishes_paths_from_abstract_names() {
    assert!(
        socket_address("/run/user/1000/kid-agentic-coding/bridge.sock")
            .expect("socket address is valid")
            .as_pathname()
            .is_some()
    );
    assert!(
        socket_address("kid-agentic-coding-workflow-1-2")
            .expect("socket address is valid")
            .as_pathname()
            .is_none()
    );
}

#[test]
fn connect_reaches_a_filesystem_socket() {
    let path = env::temp_dir().join(format!("{}.sock", unique_name("fs")));
    let _ = fs::remove_file(&path);
    let _listener = UnixListener::bind(&path).expect("bind succeeds");

    connect_to_bridge(&path.display().to_string()).expect("listening socket is reachable");

    let _ = fs::remove_file(&path);
}

#[test]
fn connect_reaches_an_abstract_socket() {
    let name = unique_name("abstract");
    let _listener = bind_abstract(&name);

    connect_to_bridge(&name).expect("listening socket is reachable");
}

#[test]
fn connect_fails_without_a_listener() {
    let err = connect_to_bridge(&unique_name("missing")).expect_err("nobody is listening");

    assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
}

#[test]
fn send_line_appends_a_newline() {
    let name = unique_name("line");
    let listener = bind_abstract(&name);
    let mut client = connect_to_bridge(&name).expect("listening socket is reachable");

    send_line(&mut client, "hello").expect("line is written");
    drop(client);

    let (mut server, _) = listener.accept().expect("accept succeeds");
    let mut received = String::new();
    server
        .read_to_string(&mut received)
        .expect("reads until the client closes");
    assert_eq!(received, "hello\n");
}

#[test]
fn bridge_error_names_the_socket_and_the_fs_socket_dir_flag() {
    let socket = unique_name("error");
    let err = io::Error::new(ErrorKind::NotFound, "no such file or directory");
    let message = bridge_error(&socket, &err);

    assert!(message.contains(&socket));
    assert!(message.contains("--fs-socket-dir"));
    assert!(message.contains("sandbox"));
}
