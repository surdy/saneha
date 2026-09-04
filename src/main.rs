use std::process::ExitCode;

fn main() -> ExitCode {
    match saneha::run() {
        // The code is the subcommand's to choose: `saneha wait` has three ways
        // of succeeding and says which by exiting 0, 3 or 4.
        Ok(code) => code,
        Err(err) => {
            // `{:#}` keeps the whole chain on one line: "context: cause".
            eprintln!("saneha: {err:#}");
            ExitCode::FAILURE
        }
    }
}
