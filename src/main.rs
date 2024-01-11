use std::{
    fs::File,
    io::{stderr, stdout, Read, Write},
    process::{Command, Stdio},
    thread,
};

fn main() {
    let mut child = Command::new("zsh")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute child");

    fn communicate(
        mut stream: impl Read,
        filename: &'static str,
        mut output: impl Write,
    ) -> std::io::Result<()> {
        let mut file = File::create(filename)?;

        let mut buf = [0u8; 1024];
        loop {
            let num_read = stream.read(&mut buf)?;
            if num_read == 0 {
                break;
            }

            let buf = &buf[..num_read];
            file.write_all(buf)?;
            output.write_all(buf)?;
        }

        Ok(())
    }

    let child_out = std::mem::take(&mut child.stdout).expect("cannot attach to child stdout");
    let child_err = std::mem::take(&mut child.stderr).expect("cannot attach to child stderr");

    let thread_out = thread::spawn(move || {
        communicate(child_out, "stdout.txt", stdout())
            .expect("error communicating with child stdout")
    });
    let thread_err = thread::spawn(move || {
        communicate(child_err, "stderr.txt", stderr())
            .expect("error communicating with child stderr")
    });

    thread_out.join().unwrap();
    thread_err.join().unwrap();

    let ecode = child.wait().expect("failed to wait on child");

    assert!(ecode.success());
}
