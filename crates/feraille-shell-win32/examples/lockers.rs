//! Print the processes holding a file open (Restart Manager smoke test).
//!
//! ```pwsh
//! cargo run -p feraille-shell-win32 --example lockers -- <path>
//! ```

fn main() {
    let path = std::env::args().nth(1).expect("usage: lockers <path> [--close]");
    let close = std::env::args().any(|a| a == "--close");
    let list = feraille_shell_win32::processes_using(std::path::Path::new(&path));
    if list.is_empty() {
        println!("no process holds {path}");
        return;
    }
    for p in &list {
        println!("{}\t{}", p.pid, p.name);
    }
    if close {
        let pids: Vec<u32> = list.iter().map(|p| p.pid).collect();
        match feraille_shell_win32::force_close_processes(&pids) {
            Ok(()) => println!("closed {pids:?}"),
            Err(e) => println!("force-close failed: {e}"),
        }
    }
}
