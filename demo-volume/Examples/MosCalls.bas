10 REM Parameterless BBC MOS CALLs use A%, X%, Y% and C%
20 REM Run with RUN $.Examples.MosCalls or BASICJIT $.Examples.MosCalls
30 DIM block% 4
40 !block%=12345:block%?4=0
50 X%=block% AND 255:Y%=block% DIV 256
60 A%=4:CALL &FFF1
70 REM OSWORD 4 sets the interval timer; OSWORD 3 reads it back
80 A%=3:CALL &FFF1
90 PRINT "Interval timer (centiseconds): ";!block%
100 A%=79:CALL &FFEE:A%=75:CALL &FFEE:CALL &FFE7
110 END
