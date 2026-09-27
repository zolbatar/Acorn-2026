# Hosted BBC MOS CALL bridge

Parameterless BASIC `CALL` recognises the MOS entrypoints below and translates
their arguments into existing caller-scoped SWI services. This is a service
adapter, not execution of 6502 or ARM machine code. It is shared by source and
tokenised BASIC; the current hybrid JIT reaches it through statement execution.
The public `SwiDispatcher::dispatch_mos_call` boundary is also available for
future native statement lowering.

| Address | MOS service | Hosted operation |
| --- | --- | --- |
| `&FFCE` | OSFIND | `OS_Find`; Y% is the close handle, X%/Y% identify the filename on open |
| `&FFD4` | OSBPUT | `OS_BPut`; A% is the byte, Y% is the handle |
| `&FFD7` | OSBGET | `OS_BGet`; Y% is the handle |
| `&FFE0` | OSRDCH | `OS_ReadC` |
| `&FFE3` | OSASCI | `OS_WriteC`, with CR expanded through `OS_NewLine` |
| `&FFE7` | OSNEWL | `OS_NewLine` |
| `&FFEE` | OSWRCH | `OS_WriteC` |
| `&FFF1` | OSWORD | `OS_Word`; A% is the reason, X%/Y% identify the block |
| `&FFF4` | OSBYTE | `OS_Byte`; A%, X%, Y% supply R0, R1, R2 |
| `&FFF7` | OSCLI | `OS_CLI`; X%/Y% identify a CR- or NUL-terminated command |

C% bit 0 supplies carry on entry. CALL does not copy returned registers into
A%, X%, Y% or C%. Use `SYS ... TO` when register results are needed, including
capturing a file handle from `OS_Find`. Memory-block results remain visible.
USR's packed-register result convention and CALL parameter lists are not
implemented by this bridge.

For a pointer, X% >= 256 is a complete logical address; otherwise the address is
X% + 256*Y%. Y% is not truncated to eight bits. Arithmetic overflow and memory
ranges are checked before accessing task memory. A full address in X% ignores
Y%, allowing traditional `X%=block%:Y%=X% DIV 256` programs as well as explicit
split-pointer code. No address is treated as a host pointer.

OSWORD reasons 1/2 read/write the system clock; 3/4 read/write the interval
timer. Blocks contain five little-endian bytes, and both clocks increment at
100 Hz modulo 2^40. BASIC TIME reads the signed low 32 bits of the same system
clock; TIME assignment sets it. Clock state belongs to the hosted dispatcher
session and survives individual BASIC runs. Interval timer events and OS_Byte
event enable/disable are not implemented. ClockSP5's `A%=4:CALL &FFF1` therefore
sets real interval-timer state, while its normal source guards still decide
whether that call is reached.

OSBYTE currently supports reason 138 inserting into keyboard buffer zero,
reason 21 flushing buffer zero, and reason 129 reading with a nonnegative
16-bit centisecond timeout (X% low byte, Y% high byte 0–127). The hosted injected
queue has capacity 256; insertion reports full via carry. Physical keyboard
matrix queries and OS-version queries are unsupported. These services share
input with OS_ReadC and BASIC's periodic key polling.

OSARGS (`&FFDA`), OSFILE (`&FFDD`) and OSGBPB (`&FFD1`) still report an explicit
unsupported-adapter error: their legacy blocks need translation to the SWI
register ABI. Other unknown addresses and unsupported OSBYTE/OSWORD reasons
also fail explicitly. This does not expand *FX hardware emulation or compile
ClockSP5's remaining interpreted code paths.

Try `RUN $.Examples.MosCalls` or `BASICJIT $.Examples.MosCalls` with the
experimental JIT feature enabled. `tests/mos_calls.rs` checks source and
tokenised calls, pointer forms above 64K, register preservation, shared clock
state, output, keyboard/CLI dispatch, and invalid buffers/addresses.

References: [BBC BASIC CALL compatibility convention](https://www.riscos.com/support/developers/bbcbasic/part3/keywords.html),
[OSWORD clock contracts](https://www.riscos.com/support/developers/prm/timedate.html),
[OSBYTE keyboard contracts](https://www.riscos.com/support/developers/prm/charinput.html),
[OSBYTE buffer contracts](https://www.riscos.com/support/developers/prm/buffers.html),
and [BASIC implementers' discussion of full and split pointers](https://stardot.org.uk/forums/viewtopic.php?start=900&t=15396).
