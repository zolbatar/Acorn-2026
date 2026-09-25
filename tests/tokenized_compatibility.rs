use acorn_2026::tokenized_basic::{TokenizedBasicProgram, TokenizedBasicRecordLayout};

struct Fixture {
    name: &'static str,
    bytes: &'static [u8],
    layout: TokenizedBasicRecordLayout,
    lines: usize,
    references: usize,
}

#[test]
fn decodes_tokenized_compatibility_fixture_corpus() {
    let fixtures = [
        Fixture {
            name: "TDU-01 public-domain BBC Micro program",
            bytes: include_bytes!("../examples/tokenized-compat/tdu-01-test.bbc"),
            layout: TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn,
            lines: 4,
            references: 0,
        },
        Fixture {
            name: "ClockSP5 ARM BASIC V compatibility program",
            bytes: include_bytes!("../examples/clocksp5/ClockSP5.bbc"),
            layout: TokenizedBasicRecordLayout::SeparateLineCarriageReturn,
            lines: 143,
            references: 37,
        },
        Fixture {
            name: "minimal tokenized echo program",
            bytes: include_bytes!("../examples/basicv-echo/echo.bbc"),
            layout: TokenizedBasicRecordLayout::SeparateLineCarriageReturn,
            lines: 3,
            references: 0,
        },
    ];

    for fixture in fixtures {
        let program = TokenizedBasicProgram::decode(fixture.bytes)
            .unwrap_or_else(|error| panic!("{} failed to decode: {error}", fixture.name));

        assert_eq!(
            program.record_layout,
            Some(fixture.layout),
            "{}",
            fixture.name
        );
        assert_eq!(program.line_count(), fixture.lines, "{}", fixture.name);
        assert_eq!(
            program.line_reference_count(),
            fixture.references,
            "{}",
            fixture.name
        );
        assert_eq!(
            program.unresolved_line_reference_count(),
            0,
            "{}",
            fixture.name
        );
    }
}
