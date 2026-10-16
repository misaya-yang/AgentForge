use ai_platform_migration_authority::{
    AuthorityError, Result,
    source_tools::measure_next_successor::{self, MeasureOptions as NextMeasureOptions},
    source_tools::measure_next_successor_022::{self, MeasureOptions as NextMeasure022Options},
    source_tools::measure_successors::{self, MeasureOptions},
};
use std::path::PathBuf;

enum Measurement {
    Historical(MeasureOptions),
    Next(NextMeasureOptions),
    Next022(NextMeasure022Options),
}

fn usage() -> &'static str {
    "measure-schema-successors [--next-successor-021|--next-successor-022] --source-root <canonical-source> --preview-root <external-preview> [--preimage-root <preserved-reference-checkout>] --expected-database schema_successor_verify_<8-48-lowercase-suffix> --run-approved-isolated-measurements"
}

fn parse_args() -> Result<Option<Measurement>> {
    let mut source_root = None;
    let mut preview_root = None;
    let mut preimage_root = None;
    let mut expected_database = None;
    let mut run_approved = false;
    let mut next_021 = false;
    let mut next_022 = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        if matches!(argument.as_str(), "--help" | "-h") {
            return Ok(None);
        }
        if argument == "--run-approved-isolated-measurements" {
            if run_approved {
                return Err(AuthorityError::Refused("duplicate approval flag".into()));
            }
            run_approved = true;
            continue;
        }
        if argument == "--next-successor-021" {
            if next_021 {
                return Err(AuthorityError::Refused(
                    "duplicate --next-successor-021 flag".into(),
                ));
            }
            next_021 = true;
            continue;
        }
        if argument == "--next-successor-022" {
            if next_022 {
                return Err(AuthorityError::Refused(
                    "duplicate --next-successor-022 flag".into(),
                ));
            }
            next_022 = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| AuthorityError::Refused("missing option value".into()))?;
        let slot = match argument.as_str() {
            "--source-root" => &mut source_root,
            "--preview-root" => &mut preview_root,
            "--preimage-root" => &mut preimage_root,
            "--expected-database" => &mut expected_database,
            _ => return Err(AuthorityError::Refused("unknown option".into())),
        };
        if slot.is_some() {
            return Err(AuthorityError::Refused("duplicate option".into()));
        }
        *slot = Some(value);
    }
    if !run_approved {
        return Err(AuthorityError::Refused(
            "measurement requires --run-approved-isolated-measurements".into(),
        ));
    }
    let source_root = PathBuf::from(
        source_root.ok_or_else(|| AuthorityError::Refused("--source-root is required".into()))?,
    );
    let preview_root = PathBuf::from(
        preview_root.ok_or_else(|| AuthorityError::Refused("--preview-root is required".into()))?,
    );
    let expected_database = expected_database
        .ok_or_else(|| AuthorityError::Refused("--expected-database is required".into()))?;
    if next_021 && next_022 {
        return Err(AuthorityError::Refused(
            "choose only one successor extension flag".into(),
        ));
    }
    if next_021 || next_022 {
        if preimage_root.is_some() {
            return Err(AuthorityError::Refused(
                "single-step successor measurement does not accept --preimage-root".into(),
            ));
        }
        if next_021 {
            Ok(Some(Measurement::Next(NextMeasureOptions {
                source_root,
                preview_root,
                expected_database,
                run_approved,
            })))
        } else {
            Ok(Some(Measurement::Next022(NextMeasure022Options {
                source_root,
                preview_root,
                expected_database,
                run_approved,
            })))
        }
    } else {
        Ok(Some(Measurement::Historical(MeasureOptions {
            source_root,
            preview_root,
            preimage_root: preimage_root.map(PathBuf::from),
            expected_database,
            run_approved,
        })))
    }
}

#[tokio::main]
async fn main() {
    match parse_args() {
        Ok(None) => println!("{}", usage()),
        Ok(Some(Measurement::Historical(options))) => {
            match measure_successors::run(options).await {
                Ok(()) => {
                    println!("measured successor changes 018-020 and verified the no-op state")
                }
                Err(error) => {
                    eprintln!("measure-schema-successors: {error}");
                    std::process::exit(1);
                }
            }
        }
        Ok(Some(Measurement::Next(options))) => match measure_next_successor::run(options).await {
            Ok(()) => println!(
                "measured successor change 021 only, verified the full epoch-21 prefix, and confirmed no-op replay"
            ),
            Err(error) => {
                eprintln!("measure-schema-successors: {error}");
                std::process::exit(1);
            }
        },
        Ok(Some(Measurement::Next022(options))) => {
            match measure_next_successor_022::run(options).await {
                Ok(()) => println!(
                    "measured successor change 022 only, verified the full epoch-22 prefix, and confirmed no-op replay"
                ),
                Err(error) => {
                    eprintln!("measure-schema-successors: {error}");
                    std::process::exit(1);
                }
            }
        }
        Err(error) => {
            eprintln!("measure-schema-successors: {error}; usage: {}", usage());
            std::process::exit(2);
        }
    }
}
