DIM mode% 52, pal% 256*4 
$MODE=STRING$(52,CHR$0) 
$mode%=STRING$(52,CHR$0) 
PROCmode("X1680 y1050 C16M EX1 EY1 F60") 
xsize%=1680:ysize%=1050:REM physical pixels 
aspect=ysize%/xsize% 
 
REM define the centre and scale factor here 
xcentre=-1.44251 
ycentre=-0.13409 
scale=0.52707 
 
xmin=xcentre-(scale/2) 
xmax=xcentre+(scale/2) 
xwidth=xmax-xmin 
ymin=ycentre+(scale*aspect/2) 
ymax=ycentre-(scale*aspect/2) 
ywidth=ymax-ymin 
max%=8192:REM maximum iterations 
FOR X%=0 TO (xsize%-1) STEP 1 
FOR Y%=0 TO (ysize%-1) STEP 1 
a=(xwidth*X%/xsize%)+xmin 
b=(ywidth*Y%/ysize%)+ymin 
REM a is the real component of complex number c 
REM b is the imaginary component of c 
PROCit(a,b,max%) 
REM apply LOG scale 
h%=360-360*LOG(IT%)/LOG(max%) 
IF (ABS(e)+ABS(f))>4 PROCsethsv(h%,&FF,&FF) ELSE SYS"ColourTrans_SetGCOL",0,,,&100,0 
MOVE X%*2,Y%*2:DRAW X%*2,Y%*2 
NEXT Y% 
NEXT X% 
REPEAT UNTIL INKEY(0)<>-1 
END 
: 
DEFPROCit(a,b,ITER%) 
IT%=0 
e=0:REM e and f are the real, imaginary parts of z 
f=0 
REPEAT 
REM z=(z*z)+c 
REM square z, result in u,v 
u=(e*e)-(f*f) 
v=2*e*f 
REM add c, put the result in e 
e=u+a 
f=v+b 
REM count it then quit or bail 
IT%=IT%+1 
UNTIL IT%=ITER% OR (ABS(e)+ABS(f))>4 
ENDPROC 
 
DEFPROCsethsv(h%,s%,v%) 
SYS"ColourTrans_ConvertHSVToRGB",h%*&10000,s%*&100,v% TO r%,g%,b% 
SYS"ColourTrans_SetGCOL",(b%<<24)+(g%<<16)+(r%<<8),,,&100,0 
ENDPROC 
 
DEFPROCmode(mode$) 
FOR i%=1 TO LENmode$ 
MID$(mode$,i%,1)=CHR$(ASC(MID$(mode$,i%,1)) OR 32):REM force A-Z to lower case 
NEXT i% 
smode$=" "+mode$ 
IF INSTR(smode$," x")=0 THEN PRINT" x pixels missing from mode string (="""mode$""") passed to PROCmode.":ENDPROC 
IF INSTR(smode$," y")=0 THEN PRINT" y pixels missing from mode string passed to PROCmode.":ENDPROC 
IF INSTR(smode$," c")=0 AND INSTR(smode$," g")=0 THEN PRINT" colours missing from mode string passed to PROCmode.":ENDPROC 
xpix%=VALRIGHT$(smode$,LENsmode$-INSTR(smode$," x")-1) 
ypix%=VALRIGHT$(smode$,LENsmode$-INSTR(smode$," y")-1) 
col_start%=INSTR(smode$," c") 
IF col_start%=0 THEN col_start%=INSTR(smode$," g") 
colour$="":col_start%+=1:offset%=0 
REPEAT 
colour$+=MID$(smode$,col_start%+offset%,1) 
offset%+=1 
UNTIL MID$(smode$,col_start%+offset%,1)=" " OR (col_start%+offset%)>LENsmode$ 
IF INSTR("c2g2c4g4c16g16c256g256c32kc16m",colour$)=0 THEN PRINT" number of colours specified in """mode$""" is not available." 
IF INSTR(smode$," f")<>0 framerate%=VALRIGHT$(smode$,LENsmode$-INSTR(smode$," f")-1) ELSE framerate%=-1 
IF INSTR(smode$," ex")<>0 THEN ex%=VALRIGHT$(smode$,LENsmode$-INSTR(smode$," ex")-2) ELSE ex%=1 
IF INSTR(smode$," ey")<>0 THEN ey%=VALRIGHT$(smode$,LENsmode$-INSTR(smode$," ey")-2) ELSE ey%=1 
REM put together mode block now. 
pos%=0 
mode%!pos%=1:pos%+=4 
mode%!pos%=xpix%:pos%+=4 
mode%!pos%=ypix%:pos%+=4 
IF colour$="c2" OR colour$="g2" THEN 
mode%!pos%=0:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
IF colour$="c4" OR colour$="g4" THEN 
mode%!pos%=1:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
IF colour$="c16" THEN 
mode%!pos%=2:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
IF colour$="g16" THEN 
mode%!pos%=2:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
IF colour$="c256" THEN 
mode%!pos%=3:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
IF colour$="g256" THEN 
mode%!pos%=3:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
mode%!pos%=0:pos%+=4 
mode%!pos%=&80:pos%+=4 
mode%!pos%=3:pos%+=4 
mode%!pos%=&FF:pos%+=4 
ENDIF 
IF colour$="c32k" THEN 
mode%!pos%=4:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
IF colour$="c16m" THEN 
mode%!pos%=5:pos%+=4 
mode%!pos%=framerate%:pos%+=4 
mode%!pos%=5:pos%+=4 
mode%!pos%=ey%:pos%+=4 
mode%!pos%=4:pos%+=4 
mode%!pos%=ex%:pos%+=4 
ENDIF 
mode%!pos%=-1 
MODE mode% 
IF colour$="g256" THEN 
FOR c%=0 TO 255 
pal%!(c%*4)=FNadd(c%,c%,c%) 
NEXT 
SYS"ColourTrans_WritePalette",-1,,pal% 
ENDIF 
IF colour$="g16" THEN 
pal%!0=FNadd(&FF,&FF,&FF) 
pal%!4=FNadd(&DD,&DD,&DD) 
pal%!8=FNadd(&BB,&BB,&BB) 
pal%!12=FNadd(&99,&99,&99) 
pal%!16=FNadd(&77,&77,&77) 
pal%!20=FNadd(&55,&55,&55) 
pal%!24=FNadd(&33,&33,&33) 
pal%!28=FNadd(&00,&00,&00) 
pal%!32=FNadd(&10,&10,&10) 
pal%!36=FNadd(&C0,&C0,&C0) 
pal%!40=FNadd(&60,&60,&60) 
pal%!44=FNadd(&20,&20,&20) 
pal%!48=FNadd(&E0,&E0,&E0) 
pal%!52=FNadd(&40,&40,&40) 
pal%!56=FNadd(&A0,&A0,&A0) 
pal%!60=FNadd(&80,&80,&80) 
SYS"ColourTrans_WritePalette",-1,,pal% 
ENDIF 
ENDPROC 
: 
DEFFNadd(r%,g%,b%) 
=(b%<<24) OR (g%<<16) OR (r%<<8)