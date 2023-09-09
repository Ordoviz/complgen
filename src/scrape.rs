use std::io::Write;
use regex::Regex;


pub fn scrape<W: Write>(input: &str, output: &mut W) -> crate::Result<()> {
    let short_long_arg_descr = Regex::new(r#"^\s*(-\S),\s*(--\S+)=(\p{Uppercase}+)\s+(.+)$"#).unwrap();
    let short_long_descr = Regex::new(r#"^\s*(-\S),\s*(--\S+)\s+(.+)$"#).unwrap();
    let long_arg_descr = Regex::new(r#"^\s*(--\S+)=(\p{Uppercase}+)\s+(.+)$"#).unwrap();
    let long_opt_arg_descr = Regex::new(r#"^\s*(--\S+)\[=(\p{Uppercase}+)\]\s+(.+)$"#).unwrap();
    let short_descr = Regex::new(r#"^\s*(-\S)\s+(.+)$"#).unwrap();

    for line in input.lines() {
        if let Some(caps) = short_long_arg_descr.captures(line) {
            let short = caps.get(1).unwrap().as_str();
            let long = caps.get(2).unwrap().as_str();
            let arg = format!("<{}>", caps.get(3).unwrap().as_str());
            let descr = caps.get(4).unwrap().as_str();
            writeln!(output, r#" | ({short} {arg} | {long}={arg}) "{descr}""#)?;
            continue;
        }

        if let Some(caps) = short_long_descr.captures(line) {
            let short = caps.get(1).unwrap().as_str();
            let long = caps.get(2).unwrap().as_str();
            let descr = caps.get(3).unwrap().as_str();
            writeln!(output, r#" | ({short} | {long}) "{descr}""#)?;
            continue;
        }

        if let Some(caps) = long_arg_descr.captures(line) {
            let long = caps.get(1).unwrap().as_str();
            let arg = format!("<{}>", caps.get(2).unwrap().as_str());
            let descr = caps.get(3).unwrap().as_str();
            writeln!(output, r#" | {long}={arg} "{descr}""#)?;
            continue;
        }

        if let Some(caps) = long_opt_arg_descr.captures(line) {
            let long = caps.get(1).unwrap().as_str();
            let arg = format!("<{}>", caps.get(2).unwrap().as_str());
            let descr = caps.get(3).unwrap().as_str();
            writeln!(output, r#" | {long}[={arg}] "{descr}""#)?;
            continue;
        }

        if let Some(caps) = short_descr.captures(line) {
            let short = caps.get(1).unwrap().as_str();
            let descr = caps.get(2).unwrap().as_str();
            writeln!(output, r#" | {short} "{descr}""#)?;
            continue;
        }
    }
    output.flush()?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn do_scrape(input: &str) -> String {
        let mut buffer: Vec<u8> = Default::default();
        scrape(input, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap()
    }

    #[test]
    fn short_long_descr() {
        const INPUT: &str = r#"-E, --extended-regexp     PATTERNS are extended regular expressions"#;
        let output = do_scrape(INPUT);
        assert_eq!(output, r#" | (-E | --extended-regexp) "PATTERNS are extended regular expressions"
"#);
    }

    #[test]
    fn short_long_arg_descr() {
        const INPUT: &str = r#"-e, --regexp=PATTERNS     use PATTERNS for matching"#;
        let output = do_scrape(INPUT);
        assert_eq!(output, r#" | (-e <PATTERNS> | --regexp=<PATTERNS>) "use PATTERNS for matching"
"#);
    }
}
