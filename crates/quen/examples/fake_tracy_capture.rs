//! A stand-in for Tracy's capture tool in quen's end-to-end tests (tests/session.rs), copied into a synthetic tools
//! folder as `tracy-capture.exe`. Without arguments it prints the usage line with its version and exits 1, as
//! tracy-capture 0.14.1 does. With `-a ADDR -p PORT -o FILE` it connects to ADDR:PORT, retrying until something listens
//! (the fake game, in a tracy launch), reads until that connection ends (the game was killed), then writes FILE and
//! exits 0: tracy-capture saves its capture when the client's connection ends.

fn main() {
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::Duration;

    let args: Vec<String> = std::env::args().collect();
    let arg = |flag: &str| {
        let at = args.iter().position(|a| a == flag)?;
        args.get(at + 1).cloned()
    };
    let (Some(addr), Some(port), Some(out)) = (arg("-a"), arg("-p"), arg("-o")) else {
        println!("tracy-capture 0.14.1 / stand-in\n");
        println!(
            "Usage: capture -o output.tracy [-a address] [-p port] [-f] [-s seconds] [-m memlimit]"
        );
        std::process::exit(1);
    };
    println!("Connecting to {addr}:{port}...");
    let mut stream = loop {
        match TcpStream::connect(format!("{addr}:{port}")) {
            Ok(stream) => break stream,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let mut got = Vec::new();
    let _ = stream.read_to_end(&mut got); // a killed game resets the connection
    std::fs::write(
        &out,
        format!(
            "synthetic tracy capture, {} bytes from the game\n",
            got.len()
        ),
    )
    .unwrap();
    println!("Saving trace... done!");
}
