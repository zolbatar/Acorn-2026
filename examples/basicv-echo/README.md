# Tokenised BASIC V echo fixture

`echo.bas` is the readable source for a minimal legacy BASIC program. `echo.bbc` is its tokenised saved-program record stream, using the shared decoder in the normal `BASIC`/`RUN` path.

The fixture runs the currently implemented compatibility subset: string-variable `INPUT`, string-variable `PRINT`, and `END`. Load and run it from the repository root:

```text
BASIC examples/basicv-echo/echo.bbc
```

Enter a line at the `? ` prompt. The program prints it back and then returns to `*`. BASIC console input and output use `OS_ReadLine`, `OS_WriteS`, `OS_Write0`, and `OS_NewLine`.
