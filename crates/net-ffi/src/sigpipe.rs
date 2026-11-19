//! A write to a connection the other side has reset raises SIGPIPE in the thread that writes,
//! and a process that does not ignore the signal dies of it. A Rust program ignores it from
//! its start; a game that loads this library does not. The library leaves the game's signals
//! alone and keeps SIGPIPE off the threads it writes on, the way libpq does.

#[cfg(unix)]
mod unix {
    fn sigpipe() -> libc::sigset_t {
        unsafe {
            let mut set = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGPIPE);
            set
        }
    }

    pub(super) fn pending() -> bool {
        unsafe {
            let mut set = std::mem::zeroed();
            libc::sigpending(&mut set);
            libc::sigismember(&set, libc::SIGPIPE) == 1
        }
    }

    /// Blocks SIGPIPE in a thread of the library for as long as the thread lives.
    pub fn block() {
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, &sigpipe(), std::ptr::null_mut());
        }
    }

    /// Blocks SIGPIPE in the game's thread for one call. On drop a SIGPIPE the call raised is
    /// taken off the thread, and the thread gets back the signals it had blocked.
    pub struct Blocked {
        before: libc::sigset_t,
        pending: bool,
    }

    impl Blocked {
        pub fn new() -> Self {
            unsafe {
                let mut before = std::mem::zeroed();
                libc::pthread_sigmask(libc::SIG_BLOCK, &sigpipe(), &mut before);
                Blocked {
                    before,
                    pending: pending(),
                }
            }
        }
    }

    impl Drop for Blocked {
        fn drop(&mut self) {
            unsafe {
                if !self.pending && pending() {
                    let mut signal = 0;
                    libc::sigwait(&sigpipe(), &mut signal);
                }
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.before, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(unix)]
pub use unix::{block, Blocked};

#[cfg(not(unix))]
pub fn block() {}

#[cfg(not(unix))]
pub struct Blocked;

#[cfg(not(unix))]
impl Blocked {
    pub fn new() -> Self {
        Blocked
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{ErrorKind, IoSlice};
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const GAME: &str = "AURIX_NET_TEST_GAME_PROCESS";

    /// A write of several buffers, as TLS and HTTP make them, to a connection the other side
    /// reset and the client has read the reset of: the write that raises SIGPIPE.
    async fn write_to_a_reset_connection() -> ErrorKind {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut stream = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        peer.set_zero_linger().unwrap();
        drop(peer);
        let reset = stream.read(&mut [0]).await.unwrap_err();
        assert_eq!(reset.kind(), ErrorKind::ConnectionReset);
        let buffers = [IoSlice::new(b"a"), IoSlice::new(b"b")];
        stream.write_vectored(&buffers).await.unwrap_err().kind()
    }

    fn blocked() -> bool {
        unsafe {
            let mut now = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut now);
            libc::sigismember(&now, libc::SIGPIPE) == 1
        }
    }

    /// What a game does with the library: SIGPIPE kills its process, and it calls in from a
    /// thread of its own.
    fn game() {
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        let on_the_library_thread = crate::rt().spawn(write_to_a_reset_connection());
        let written = crate::block_on(on_the_library_thread).unwrap();
        assert_eq!(written, ErrorKind::BrokenPipe);

        let written = crate::block_on(write_to_a_reset_connection());
        assert_eq!(written, ErrorKind::BrokenPipe);
        assert!(!blocked() && !super::unix::pending());
    }

    #[test]
    fn a_write_to_a_reset_connection_does_not_kill_the_game() {
        if std::env::var_os(GAME).is_some() {
            return game();
        }
        let test = "sigpipe::tests::a_write_to_a_reset_connection_does_not_kill_the_game";
        let game = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test])
            .env(GAME, "1")
            .output()
            .unwrap();
        assert_eq!(game.status.signal(), None, "a signal killed the game");
        let said = String::from_utf8_lossy(&game.stdout);
        assert!(game.status.success(), "{said}");
    }
}
