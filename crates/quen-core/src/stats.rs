//! PresentMon 2.x: one window's command line, its CSV stats, and the within-boot A/B verdict.
//!
//! The header row names the columns, matched without regard to case. Cells may be empty or `NA`; both are skipped. The
//! first [`SKIP_ROWS`] data rows are dropped because they can straddle the window's start (parity with the Python).
//! Percentiles interpolate linearly between ranks, as numpy's default does.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;

/// Data rows dropped from the start of every capture.
pub const SKIP_ROWS: usize = 5;

const FRAME: &str = "MsBetweenPresents";
const GPU_BUSY: &str = "MsGPUBusy";
const GPU_TIME: &str = "MsGPUTime";
const CPU_BUSY: &str = "MsCPUBusy";
const PRESENT_MODE: &str = "PresentMode";

/// One column's numeric cells after the drop.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub median: f64,
    pub p95: f64,
}

/// One PresentMon window. An optional column is `None` when the header lacks it or no number is left in it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    /// MsBetweenPresents.
    pub frame_ms: Summary,
    /// MsGPUBusy.
    pub gpu_busy_ms: Option<Summary>,
    /// MsGPUTime.
    pub gpu_time_ms: Option<Summary>,
    /// MsCPUBusy.
    pub cpu_busy_ms: Option<Summary>,
    /// The most common PresentMode, the first seen on a tie; `None` when the column is missing or empty.
    pub present_mode: Option<String>,
    /// More than one PresentMode among the kept rows.
    pub present_mode_mixed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Verdict {
    #[serde(rename = "inside drift")]
    InsideDrift,
    #[serde(rename = "outside drift")]
    OutsideDrift,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AbVerdict {
    /// Per label, the spread of its windows' mean frame times: |first − second| for the usual two windows.
    pub drift_ms: BTreeMap<String, f64>,
    /// The mean of the second label's window means minus the first label's.
    pub diff_ms: f64,
    pub verdict: Verdict,
    /// The PresentMode every window ran in.
    pub present_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not a CSV table: no header, an unterminated quote, or a row whose width differs from the header's.
    Csv {
        line: usize,
        msg: String,
    },
    MissingColumn(&'static str),
    /// A cell of a used column is neither a finite number nor empty or `NA`.
    BadNumber {
        line: usize,
        column: &'static str,
        cell: String,
    },
    /// No frame time is left after the first [`SKIP_ROWS`] data rows.
    NoFrames {
        rows: usize,
    },
    /// The windows don't form an A/B run: not exactly two labels, or a label with one window.
    Order(String),
    /// A window's PresentMode is unknown, mixed, or differs from the others.
    PresentMode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Csv { line, msg } => write!(f, "CSV line {line}: {msg}"),
            Error::MissingColumn(name) => write!(f, "the CSV has no {name} column"),
            Error::BadNumber { line, column, cell } => {
                write!(f, "CSV line {line}: {column} {cell:?} is not a number")
            }
            Error::NoFrames { rows } => write!(
                f,
                "no frame time after dropping {SKIP_ROWS} of {rows} data rows"
            ),
            Error::Order(msg) => write!(f, "not an A/B run: {msg}"),
            Error::PresentMode(msg) => write!(f, "present mode: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// PresentMon's command line for one window of `secs` on the process `pid`, its CSV to `csv`. The flags are
/// `ini_session.py perf`'s, with two differences: the target is the pid, never an image name (that would also record
/// another copy of the game), and the trace session is quen's own (`--session_name quen --stop_existing_session`), so a
/// session a killed run left can't block the next window and the user's own PresentMon session is never touched. `extra`
/// (perf's `W3_PM_FLAGS`) may hold only `--track_*` / `--no_track_*` switches, e.g. `--track_pc_latency` (the column
/// that tells real frames from generated ones) or `--track_frame_type`: the rest would change what or where it records.
pub fn presentmon_args(
    exe: &str,
    pid: u32,
    secs: u32,
    csv: &str,
    extra: &[String],
) -> Result<Vec<String>, String> {
    let switch = |f: &str| {
        let name = f.strip_prefix("--no_track_").or(f.strip_prefix("--track_"));
        name.is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
    };
    if let Some(bad) = extra.iter().find(|f| !switch(f)) {
        return Err(format!(
            "PresentMon flag {bad:?}: only --track_* and --no_track_* switches (quen sets the rest)"
        ));
    }
    let (pid, secs) = (pid.to_string(), secs.to_string());
    let mut argv: Vec<String> = [
        exe,
        "--process_id",
        &pid,
        "--timed",
        &secs,
        "--terminate_after_timed",
        "--output_file",
        csv,
        "--no_console_stats",
        "--session_name",
        "quen",
        "--stop_existing_session",
    ]
    .map(String::from)
    .into();
    argv.extend_from_slice(extra);
    Ok(argv)
}

/// The stats of one PresentMon CSV.
pub fn parse(csv: &str) -> Result<Stats, Error> {
    let mut records = records(csv)?.into_iter();
    let (_, header) = records.next().ok_or_else(|| Error::Csv {
        line: 1,
        msg: "no header row".into(),
    })?;
    let rows: Vec<_> = records.collect();
    if let Some((line, row)) = rows.iter().find(|(_, row)| row.len() != header.len()) {
        let msg = format!("{} fields, the header has {}", row.len(), header.len());
        return Err(Error::Csv { line: *line, msg });
    }
    let kept = rows.get(SKIP_ROWS..).unwrap_or_default();
    let column = |name: &str| {
        header
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
    };
    let cells = |i: usize| {
        kept.iter()
            .map(move |(line, row)| (*line, row[i].trim()))
            .filter(|(_, cell)| !cell.is_empty() && *cell != "NA")
    };
    let summary = |name: &'static str| -> Result<Option<Summary>, Error> {
        let Some(i) = column(name) else {
            return Ok(None);
        };
        let mut values = Vec::new();
        for (line, cell) in cells(i) {
            match cell.parse::<f64>() {
                Ok(v) if v.is_finite() => values.push(v),
                _ => {
                    return Err(Error::BadNumber {
                        line,
                        column: name,
                        cell: cell.into(),
                    });
                }
            }
        }
        Ok(summarise(values))
    };

    if column(FRAME).is_none() {
        return Err(Error::MissingColumn(FRAME));
    }
    let frame_ms = summary(FRAME)?.ok_or(Error::NoFrames { rows: rows.len() })?;
    let mut modes: Vec<(&str, usize)> = Vec::new();
    for (_, mode) in column(PRESENT_MODE).into_iter().flat_map(cells) {
        match modes.iter_mut().find(|(m, _)| *m == mode) {
            Some((_, n)) => *n += 1,
            None => modes.push((mode, 1)),
        }
    }
    // max_by_key returns the last of equal maxima; reversed, that is the first seen.
    let most = modes.iter().rev().max_by_key(|(_, n)| *n);
    Ok(Stats {
        frame_ms,
        gpu_busy_ms: summary(GPU_BUSY)?,
        gpu_time_ms: summary(GPU_TIME)?,
        cpu_busy_ms: summary(CPU_BUSY)?,
        present_mode: most.map(|(m, _)| m.to_string()),
        present_mode_mixed: modes.len() > 1,
    })
}

/// The verdict of an ordered run such as A,B,A,B or A,B,B,A. The first window's label is the baseline A; there must be
/// one other label, and each label needs two or more windows. "Outside drift" only if |diff| exceeds every label's drift.
pub fn ab_verdict<L: AsRef<str>>(windows: &[(L, Stats)]) -> Result<AbVerdict, Error> {
    let mut labels: Vec<(&str, Vec<f64>)> = Vec::new();
    let mut present_mode: Option<&str> = None;
    for (label, stats) in windows {
        let label = label.as_ref();
        let mode = match (&stats.present_mode, stats.present_mode_mixed) {
            (Some(mode), false) => mode.as_str(),
            (Some(_), true) => {
                return Err(Error::PresentMode(format!("window {label} mixed several")));
            }
            (None, _) => return Err(Error::PresentMode(format!("window {label} has none"))),
        };
        if let Some(first) = present_mode
            && first != mode
        {
            return Err(Error::PresentMode(format!(
                "window {label} ran in {mode:?}, an earlier one in {first:?}"
            )));
        }
        present_mode = Some(mode);
        match labels.iter_mut().find(|(l, _)| *l == label) {
            Some((_, means)) => means.push(stats.frame_ms.mean),
            None => labels.push((label, vec![stats.frame_ms.mean])),
        }
    }
    let [(_, a), (_, b)] = labels.as_slice() else {
        return Err(Error::Order(format!(
            "{} labels, expected two",
            labels.len()
        )));
    };
    if let Some((label, _)) = labels.iter().find(|(_, means)| means.len() < 2) {
        return Err(Error::Order(format!(
            "label {label} has one window, drift needs two"
        )));
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let spread = |v: &[f64]| {
        v.iter().copied().fold(f64::MIN, f64::max) - v.iter().copied().fold(f64::MAX, f64::min)
    };
    let diff_ms = mean(b) - mean(a);
    let drift_ms: BTreeMap<String, f64> = labels
        .iter()
        .map(|(l, means)| (l.to_string(), spread(means)))
        .collect();
    let outside = drift_ms.values().all(|drift| diff_ms.abs() > *drift);
    Ok(AbVerdict {
        drift_ms,
        diff_ms,
        verdict: if outside {
            Verdict::OutsideDrift
        } else {
            Verdict::InsideDrift
        },
        present_mode: present_mode.unwrap_or_default().to_string(),
    })
}

/// Count, mean, median and p95 (linear between ranks, numpy's default) of `values`; None when empty.
pub fn summarise(mut values: Vec<f64>) -> Option<Summary> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let percentile = |q: f64| {
        let rank = q * (values.len() - 1) as f64;
        let (lo, hi) = (values[rank.floor() as usize], values[rank.ceil() as usize]);
        lo + (hi - lo) * rank.fract()
    };
    Some(Summary {
        count: values.len(),
        mean: values.iter().sum::<f64>() / values.len() as f64,
        median: percentile(0.5),
        p95: percentile(0.95),
    })
}

/// The non-blank records of a CSV text, each with the line it starts on. Handles quoted fields (`""` is a quote, and
/// commas and line breaks inside quotes are kept), CRLF and a UTF-8 BOM.
fn records(text: &str) -> Result<Vec<(usize, Vec<String>)>, Error> {
    let mut out = Vec::new();
    let (mut record, mut field) = (Vec::new(), String::new());
    let (mut line, mut start, mut quoted) = (1, 1, false);
    let mut chars = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .chars()
        .peekable();
    let mut end_record = |record: &mut Vec<String>, field: &mut String, start: usize| {
        record.push(std::mem::take(field));
        let record = std::mem::take(record);
        if record != [""] {
            out.push((start, record));
        }
    };
    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
        }
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, _) => field.push(c),
            (false, '"') if field.is_empty() => quoted = true,
            (false, ',') => record.push(std::mem::take(&mut field)),
            (false, '\r') => {}
            (false, '\n') => {
                end_record(&mut record, &mut field, start);
                start = line;
            }
            (false, _) => field.push(c),
        }
    }
    if quoted {
        return Err(Error::Csv {
            line: start,
            msg: "unterminated quote".into(),
        });
    }
    end_record(&mut record, &mut field, start);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,\
                          AllowsTearing,PresentMode,FrameType,CPUStartTime,MsBetweenSimulationStart,MsBetweenPresents,\
                          MsBetweenDisplayChange,MsInPresentAPI,MsUntilDisplayed,MsCPUBusy,MsCPUWait,MsGPULatency,\
                          MsGPUTime,MsGPUBusy,MsGPUWait";
    const FLIP: &str = "Hardware: Independent Flip";

    /// A synthetic PresentMon CSV: five warm-up rows of 999 ms, then one row per frame time. GPU busy is frame − 2,
    /// GPU time frame − 1, CPU busy frame / 2.
    fn csv(frames: &[f64], mode: &str) -> String {
        let row = |ms: f64| {
            format!(
                "game.exe,4242,0x0000000000ABCDEF,DXGI,0,0,1,{mode},Application,1234.5,NA,{ms},{ms},0.1,{ms},{},0.2,NA,{},{},0.3\n",
                ms / 2.0,
                ms - 1.0,
                ms - 2.0
            )
        };
        let mut out = format!("{HEADER}\n");
        for _ in 0..SKIP_ROWS {
            out += &row(999.0);
        }
        for &ms in frames {
            out += &row(ms);
        }
        out
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn presentmon_args_take_the_pid_and_only_track_switches() {
        let extra = ["--track_pc_latency".to_string(), "--no_track_input".into()];
        assert_eq!(
            presentmon_args("pm.exe", 4242, 15, "a b.csv", &extra).unwrap(),
            [
                "pm.exe",
                "--process_id",
                "4242",
                "--timed",
                "15",
                "--terminate_after_timed",
                "--output_file",
                "a b.csv",
                "--no_console_stats",
                "--session_name",
                "quen",
                "--stop_existing_session",
                "--track_pc_latency",
                "--no_track_input",
            ]
        );
        for bad in [
            "--process_name",
            "--output_stdout",
            "--track_",
            "--track_PC",
            "-track_x",
            "",
        ] {
            let err = presentmon_args("pm.exe", 1, 1, "x.csv", &[bad.into()]).unwrap_err();
            assert!(err.contains("only --track_*"), "{bad}: {err}");
        }
    }

    #[test]
    fn summarises_after_the_warm_up_rows() {
        let frames: Vec<f64> = (1..=10).map(f64::from).collect();
        let stats = parse(&csv(&frames, FLIP)).unwrap();
        let f = &stats.frame_ms;
        assert_eq!(f.count, 10);
        assert!(
            close(f.mean, 5.5) && close(f.median, 5.5) && close(f.p95, 9.55),
            "{f:?}"
        );
        let gpu = stats.gpu_busy_ms.unwrap();
        assert!(
            close(gpu.mean, 3.5) && close(gpu.median, 3.5) && close(gpu.p95, 7.55),
            "{gpu:?}"
        );
        assert!(close(stats.gpu_time_ms.unwrap().mean, 4.5));
        assert!(close(stats.cpu_busy_ms.unwrap().mean, 2.75));
        assert_eq!(stats.present_mode.as_deref(), Some(FLIP));
        assert!(!stats.present_mode_mixed);

        let one = parse(&csv(&[16.0], FLIP)).unwrap().frame_ms;
        assert_eq!(
            one,
            Summary {
                count: 1,
                mean: 16.0,
                median: 16.0,
                p95: 16.0
            }
        );
        let odd = parse(&csv(&[3.0, 1.0, 2.0], FLIP)).unwrap().frame_ms;
        assert!(close(odd.median, 2.0) && close(odd.p95, 2.9), "{odd:?}");
    }

    #[test]
    fn skips_na_and_empty_cells_and_absent_columns() {
        let text = "\
MsBetweenPresents,MsGPUBusy,PresentMode,MsCPUBusy
1,1,Composed: Flip,NA
1,1,Composed: Flip,NA
1,1,Composed: Flip,NA
1,1,Composed: Flip,NA
1,1,Composed: Flip,NA
10,NA,Composed: Flip,NA
20,,NA,
NA,4, ,NA
30,6,,
";
        let stats = parse(text).unwrap();
        assert_eq!(stats.frame_ms.count, 3);
        assert!(close(stats.frame_ms.mean, 20.0));
        assert_eq!(stats.gpu_busy_ms.unwrap().count, 2);
        assert_eq!(stats.gpu_time_ms, None);
        assert_eq!(stats.cpu_busy_ms, None);
        assert_eq!(stats.present_mode.as_deref(), Some("Composed: Flip"));
        assert!(!stats.present_mode_mixed);
    }

    #[test]
    fn reads_crlf_quotes_bom_and_any_header_case() {
        let lf = csv(&[10.0, 12.0, 14.0], FLIP);
        let expected = parse(&lf).unwrap();
        assert_eq!(parse(&lf.replace('\n', "\r\n")).unwrap(), expected);
        let quoted = lf.replacen("PresentMode", "\"PresentMode\"", 1).replacen(
            "MsBetweenPresents",
            "\"MsBetweenPresents\"",
            1,
        );
        assert_eq!(parse(&format!("\u{feff}{quoted}")).unwrap(), expected);
        assert_eq!(
            parse(&lf.replacen("MsBetweenPresents", "msbetweenpresents", 1)).unwrap(),
            expected
        );
        // A quoted cell with a comma, an escaped quote and a line break, and no newline at the end.
        let text = "PresentMode,\"MsBetweenPresents\"\r\n".to_string()
            + &"x,1\n".repeat(SKIP_ROWS)
            + "\"Mode, \"\"odd\"\"\nline\",2\n\n\"Mode, \"\"odd\"\"\nline\",4";
        let stats = parse(&text).unwrap();
        assert!(close(stats.frame_ms.mean, 3.0));
        assert_eq!(stats.present_mode.as_deref(), Some("Mode, \"odd\"\nline"));
    }

    #[test]
    fn reports_the_most_common_present_mode_and_mixed() {
        let mut text = csv(&[10.0, 10.0, 10.0], FLIP);
        text += &csv(&[10.0, 10.0], "Composed: Flip")
            .lines()
            .skip(1 + SKIP_ROWS)
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        let stats = parse(&text).unwrap();
        assert_eq!(stats.present_mode.as_deref(), Some(FLIP));
        assert!(stats.present_mode_mixed);
        // A tie goes to the first seen.
        let tie = "PresentMode,MsBetweenPresents\n".to_string()
            + &"w,1\n".repeat(SKIP_ROWS)
            + "b,1\na,1\na,1\nb,1\n";
        let stats = parse(&tie).unwrap();
        assert_eq!(stats.present_mode.as_deref(), Some("b"));
        assert!(stats.present_mode_mixed);
        // The warm-up rows don't count.
        let warm =
            "PresentMode,MsBetweenPresents\n".to_string() + &"w,1\n".repeat(SKIP_ROWS) + "a,1\n";
        assert!(!parse(&warm).unwrap().present_mode_mixed);
    }

    #[test]
    fn refuses_what_it_cannot_measure() {
        assert_eq!(
            parse(""),
            Err(Error::Csv {
                line: 1,
                msg: "no header row".into()
            })
        );
        assert_eq!(
            parse("MsGPUBusy,PresentMode\n1,a\n"),
            Err(Error::MissingColumn("MsBetweenPresents"))
        );
        assert_eq!(
            parse(&csv(&[], FLIP)),
            Err(Error::NoFrames { rows: SKIP_ROWS })
        );
        assert_eq!(
            parse("MsBetweenPresents\n1\n2\n"),
            Err(Error::NoFrames { rows: 2 })
        );
        let ragged = csv(&[1.0, 2.0], FLIP) + "game.exe,1\n";
        assert!(
            matches!(parse(&ragged), Err(Error::Csv { line: 9, .. })),
            "{:?}",
            parse(&ragged)
        );
        assert!(matches!(
            parse("MsBetweenPresents\n\"1\n"),
            Err(Error::Csv { line: 2, .. })
        ));
        let bad = "MsBetweenPresents,MsGPUBusy\n".to_string()
            + &"1,1\n".repeat(SKIP_ROWS)
            + "1,1\n2,fast\n";
        assert_eq!(
            parse(&bad),
            Err(Error::BadNumber {
                line: 8,
                column: "MsGPUBusy",
                cell: "fast".into()
            })
        );
        for cell in ["inf", "NaN", "1e999", "na"] {
            let text = "MsBetweenPresents\n".to_string() + &"1\n".repeat(SKIP_ROWS) + cell + "\n";
            assert!(
                matches!(parse(&text), Err(Error::BadNumber { .. })),
                "{cell}"
            );
        }
        // A bad cell in a dropped warm-up row is not read.
        let warm = "MsBetweenPresents\nbad\n".to_string() + &"1\n".repeat(SKIP_ROWS);
        assert_eq!(parse(&warm).unwrap().frame_ms.count, 1);
    }

    fn window(frame_mean: f64) -> Stats {
        parse(&csv(&[frame_mean - 1.0, frame_mean + 1.0], FLIP)).unwrap()
    }

    #[test]
    fn verdict_compares_the_difference_with_every_drift() {
        let run = [
            ("A", window(23.0)),
            ("B", window(21.0)),
            ("A", window(23.3)),
            ("B", window(21.2)),
        ];
        let v = ab_verdict(&run).unwrap();
        assert!(
            close(v.drift_ms["A"], 0.3) && close(v.drift_ms["B"], 0.2),
            "{v:?}"
        );
        assert!(close(v.diff_ms, -2.05), "{v:?}");
        assert_eq!(v.verdict, Verdict::OutsideDrift);
        assert_eq!(v.present_mode, FLIP);
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["verdict"], "outside drift");

        // A drift larger than the difference keeps it inside, whichever label drifts.
        let run = [
            ("A", window(23.0)),
            ("B", window(22.8)),
            ("A", window(23.1)),
            ("B", window(23.4)),
        ];
        assert_eq!(ab_verdict(&run).unwrap().verdict, Verdict::InsideDrift);
        // A difference equal to a drift is inside.
        let run = [
            ("A", window(10.0)),
            ("B", window(11.0)),
            ("A", window(11.0)),
            ("B", window(12.0)),
        ];
        let v = ab_verdict(&run).unwrap();
        assert!(close(v.diff_ms, 1.0) && close(v.drift_ms["A"], 1.0));
        assert_eq!(v.verdict, Verdict::InsideDrift);
        // A,B,B,A with String labels; B is the label that comes second.
        let run = [
            ("off".to_string(), window(20.0)),
            ("on".to_string(), window(25.0)),
            ("on".to_string(), window(25.5)),
            ("off".to_string(), window(20.25)),
        ];
        let v = ab_verdict(&run).unwrap();
        assert!(
            close(v.diff_ms, 5.125)
                && close(v.drift_ms["off"], 0.25)
                && close(v.drift_ms["on"], 0.5),
            "{v:?}"
        );
        assert_eq!(v.verdict, Verdict::OutsideDrift);
        // Three windows of a label: the spread of all three.
        let run = [
            ("A", window(10.0)),
            ("B", window(20.0)),
            ("A", window(10.4)),
            ("B", window(20.0)),
            ("A", window(9.9)),
        ];
        assert!(close(ab_verdict(&run).unwrap().drift_ms["A"], 0.5));
    }

    #[test]
    fn verdict_refuses_other_runs_and_present_modes() {
        let order = |run: &[(&str, Stats)]| matches!(ab_verdict(run), Err(Error::Order(_)));
        assert!(order(&[]));
        assert!(order(&[("A", window(1.0)), ("A", window(1.0))]));
        assert!(order(&[
            ("A", window(1.0)),
            ("B", window(1.0)),
            ("A", window(1.0))
        ]));
        assert!(order(&[
            ("A", window(1.0)),
            ("B", window(1.0)),
            ("C", window(1.0)),
            ("A", window(1.0)),
            ("B", window(1.0)),
            ("C", window(1.0))
        ]));

        let mut composed = window(10.0);
        composed.present_mode = Some("Composed: Flip".into());
        let run = [
            ("A", window(10.0)),
            ("B", window(12.0)),
            ("A", window(10.0)),
            ("B", composed.clone()),
        ];
        assert!(matches!(ab_verdict(&run), Err(Error::PresentMode(_))));
        let mut mixed = window(12.0);
        mixed.present_mode_mixed = true;
        let run = [
            ("A", window(10.0)),
            ("B", mixed),
            ("A", window(10.0)),
            ("B", window(12.0)),
        ];
        assert!(matches!(ab_verdict(&run), Err(Error::PresentMode(_))));
        let mut unknown = window(10.0);
        unknown.present_mode = None;
        let run = [
            ("A", unknown),
            ("B", window(12.0)),
            ("A", window(10.0)),
            ("B", window(12.0)),
        ];
        assert!(matches!(ab_verdict(&run), Err(Error::PresentMode(_))));
        // Every window in the same other mode is fine.
        let run = [
            ("A", composed.clone()),
            ("B", composed.clone()),
            ("A", composed.clone()),
            ("B", composed),
        ];
        assert_eq!(ab_verdict(&run).unwrap().present_mode, "Composed: Flip");
    }
}
