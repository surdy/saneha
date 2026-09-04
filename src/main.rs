use std::process::ExitCode;

fn main() -> ExitCode {
    match saneha::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` keeps the whole chain on one line: "context: cause".
            eprintln!("saneha: {err:#}");
            ExitCode::FAILURE
        }
    }
}
