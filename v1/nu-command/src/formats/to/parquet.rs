use nu_protocol::ast::Call;
use nu_protocol::engine::{Command, EngineState, Stack};
use nu_protocol::{
    Category, Config, Example, IntoPipelineData, PipelineData, ShellError, Signature, Span, Type,
    Value,
};

use csv::WriterBuilder;
use indexmap::{indexset, IndexSet};
//use nu_protocol::{IntoPipelineData, PipelineData, ShellError, Span, Value};
use std::collections::VecDeque;

use arrow::csv::ReaderBuilder;
use parquet::{arrow::ArrowWriter, errors::ParquetError};
use std::fs::File;
use std::sync::Arc;

#[derive(Clone)]
pub struct ToParquet;

impl Command for ToParquet {
    fn name(&self) -> &str {
        "to parquet"
    }

    fn signature(&self) -> Signature {
        Signature::build("to parquet")
            .input_output_types(vec![(Type::Any, Type::String)])
            .switch(
                "noheaders",
                "do not output the columns names as the first row",
                Some('n'),
            )
            .category(Category::Formats)
    }

    fn examples(&self) -> Vec<Example> {
        vec![
            Example {
                description: "Outputs an CSV string representing the contents of this table",
                example: "[[foo bar]; [1 2]] | to csv",
                result: Some(Value::test_string("foo,bar\n1,2\n")),
            },
            Example {
                description: "Outputs an CSV string representing the contents of this table",
                example: "[[foo bar]; [1 2]] | to csv -s ';' ",
                result: Some(Value::test_string("foo;bar\n1;2\n")),
            },
        ]
    }

    fn usage(&self) -> &str {
        "Convert table into a parquet file "
    }

    fn run(
        &self,
        engine_state: &EngineState,
        _stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<nu_protocol::PipelineData, ShellError> {
        let head = call.head;
        let noheaders = call.has_flag("noheaders");
        let config = engine_state.get_config();
        to_parquet(input, noheaders, head, config)
    }
}

fn to_parquet(
    input: PipelineData,
    noheaders: bool,
    head: Span,
    config: &Config,
) -> Result<PipelineData, ShellError> {
    let sep: char = ',';
    to_delimited_data_for_parquet(noheaders, sep, "CSV", input, head, config)
}

fn from_value_to_delimited_string(
    value: &Value,
    separator: char,
    config: &Config,
    head: Span,
) -> Result<String, ShellError> {
    match value {
        Value::Record { cols, vals, span } => {
            let mut wtr = WriterBuilder::new()
                .delimiter(separator as u8)
                .from_writer(vec![]);
            let mut fields: VecDeque<String> = VecDeque::new();
            let mut values: VecDeque<String> = VecDeque::new();

            for (k, v) in cols.iter().zip(vals.iter()) {
                fields.push_back(k.clone());

                values.push_back(to_string_tagged_value(v, config, *span)?);
            }

            wtr.write_record(fields).expect("can not write.");
            wtr.write_record(values).expect("can not write.");

            let v = String::from_utf8(wtr.into_inner().map_err(|_| {
                ShellError::UnsupportedInput("Could not convert record".to_string(), *span)
            })?)
            .map_err(|_| {
                ShellError::UnsupportedInput("Could not convert record".to_string(), *span)
            })?;
            Ok(v)
        }
        Value::List { vals, span } => {
            let mut wtr = WriterBuilder::new()
                .delimiter(separator as u8)
                .from_writer(vec![]);

            let merged_descriptors = merge_descriptors(vals);

            if merged_descriptors.is_empty() {
                wtr.write_record(
                    vals.iter()
                        .map(|ele| {
                            to_string_tagged_value(ele, config, *span)
                                .unwrap_or_else(|_| String::new())
                        })
                        .collect::<Vec<_>>(),
                )
                .expect("can not write");
            } else {
                wtr.write_record(merged_descriptors.iter().map(|item| &item[..]))
                    .expect("can not write.");

                for l in vals {
                    let mut row = vec![];
                    for desc in &merged_descriptors {
                        row.push(match l.to_owned().get_data_by_key(desc) {
                            Some(s) => to_string_tagged_value(&s, config, *span)?,
                            None => String::new(),
                        });
                    }
                    wtr.write_record(&row).expect("can not write");
                }
            }
            let v = String::from_utf8(wtr.into_inner().map_err(|_| {
                ShellError::UnsupportedInput("Could not convert record".to_string(), *span)
            })?)
            .map_err(|_| {
                ShellError::UnsupportedInput("Could not convert record".to_string(), *span)
            })?;
            Ok(v)
        }
        _ => to_string_tagged_value(value, config, head),
    }
}

fn to_string_tagged_value(v: &Value, config: &Config, span: Span) -> Result<String, ShellError> {
    match &v {
        Value::String { .. }
        | Value::Bool { .. }
        | Value::Int { .. }
        | Value::Duration { .. }
        | Value::Binary { .. }
        | Value::CustomValue { .. }
        | Value::Error { .. }
        | Value::Filesize { .. }
        | Value::CellPath { .. }
        | Value::List { .. }
        | Value::Record { .. }
        | Value::Float { .. } => Ok(v.clone().into_abbreviated_string(config)),
        Value::Date { val, .. } => Ok(val.to_string()),
        Value::Nothing { .. } => Ok(String::new()),
        _ => Err(ShellError::UnsupportedInput(
            "Unexpected value".to_string(),
            v.span().unwrap_or(span),
        )),
    }
}

pub fn merge_descriptors(values: &[Value]) -> Vec<String> {
    let mut ret: Vec<String> = vec![];
    let mut seen: IndexSet<String> = indexset! {};
    for value in values {
        let data_descriptors = match value {
            Value::Record { cols, .. } => cols.to_owned(),
            _ => vec!["".to_string()],
        };
        for desc in data_descriptors {
            if !desc.is_empty() && !seen.contains(&desc) {
                seen.insert(desc.to_string());
                ret.push(desc.to_string());
            }
        }
    }
    ret
}

pub fn to_delimited_data_for_parquet(
    noheaders: bool,
    sep: char,
    format_name: &'static str,
    input: PipelineData,
    span: Span,
    config: &Config,
) -> Result<PipelineData, ShellError> {
    let value = input.into_value(span);
    let output = match from_value_to_delimited_string(&value, sep, config, span) {
        Ok(mut x) => {
            if noheaders {
                if let Some(second_line) = x.find('\n') {
                    let start = second_line + 1;
                    x.replace_range(0..start, "");
                }
            }
            Ok(x)
        }
        Err(_) => Err(ShellError::CantConvert(
            format_name.into(),
            value.get_type().to_string(),
            value.span().unwrap_or(span),
            None,
        )),
    }?;

    let _why = parquet_file_writer(&output);

    // This works and returns nothing...
    // Ok(Value::Nothing { span: span }.into_pipeline_data())

    // This was the original way it worked
    // Ok(Value::string(output, span).into_pipeline_data())
    Ok(Value::string("Saved parquet file", span).into_pipeline_data())
}

pub fn parquet_file_writer(csv: &str) -> Result<(), ParquetError> {
    let data = csv.as_bytes();
    let mut cursor = std::io::Cursor::new(data);

    let delimiter: char = ',';

    let schema =
        match arrow::csv::reader::infer_file_schema(&mut cursor, delimiter as u8, None, true) {
            Ok((schema, _inferred_has_header)) => Ok(schema),
            Err(error) => Err(ParquetError::General(format!(
                "Error inferring schema: {}",
                error
            ))),
        }?;

    let schema_ref = Arc::new(schema);

    let builder = ReaderBuilder::new()
        .has_header(true)
        .with_delimiter(delimiter as u8)
        .with_schema(schema_ref);

    let reader = builder.build(cursor)?;

    let output = File::create("foo.parquet")?;

    let mut writer = ArrowWriter::try_new(output, reader.schema(), None)?;

    for batch in reader {
        match batch {
            Ok(batch) => writer.write(&batch)?,
            Err(error) => return Err(error.into()),
        }
    }

    match writer.close() {
        Ok(_) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_examples() {
        use crate::test_examples;

        test_examples(ToParquet {})
    }
}
