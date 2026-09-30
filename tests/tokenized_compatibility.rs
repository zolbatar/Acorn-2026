use ricochet::tokenized_basic::{TokenizedBasicProgram, TokenizedBasicRecordLayout};

struct Fixture {
    name: &'static str,
    bytes: &'static [u8],
    layout: TokenizedBasicRecordLayout,
    lines: usize,
    references: usize,
    first_line_bytes: Option<&'static [u8]>,
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
            first_line_bytes: None,
        },
        Fixture {
            name: "TETRIZ 1.5 BBC Master BASIC IV-class program",
            bytes: include_bytes!("../examples/tokenized-compat/tetriz-master-basic4.bbc"),
            layout: TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn,
            lines: 362,
            references: 2,
            first_line_bytes: Some(b"\xF4 >MTETRIZ15"),
        },
        Fixture {
            name: "synthetic shared-boundary PRINT/END core fixture",
            bytes: include_bytes!("../examples/tokenized-compat/classic-core-smoke.bbc"),
            layout: TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn,
            lines: 2,
            references: 0,
            first_line_bytes: Some(b"\xF1 \"LEGACY\""),
        },
        Fixture {
            name: "ClockSP5 ARM BASIC V compatibility program",
            bytes: include_bytes!("../examples/clocksp5/ClockSP5.bbc"),
            layout: TokenizedBasicRecordLayout::SeparateLineCarriageReturn,
            lines: 143,
            references: 37,
            first_line_bytes: None,
        },
        Fixture {
            name: "minimal tokenized echo program",
            bytes: include_bytes!("../examples/basicv-echo/echo.bbc"),
            layout: TokenizedBasicRecordLayout::SeparateLineCarriageReturn,
            lines: 3,
            references: 0,
            first_line_bytes: None,
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
        if let Some(expected) = fixture.first_line_bytes {
            assert_eq!(
                program.lines.first().map(|line| line.bytes.as_slice()),
                Some(expected),
                "{} token bytes were preserved",
                fixture.name
            );
        }
    }
}
