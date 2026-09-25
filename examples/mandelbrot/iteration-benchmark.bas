10 xsize%=80
20 ysize%=50
30 aspect=ysize%/xsize%
40 xcentre=-1.44251
50 ycentre=-0.13409
60 scale=0.52707
70 xmin=xcentre-(scale/2)
80 xmax=xcentre+(scale/2)
90 xwidth=xmax-xmin
100 ymin=ycentre+(scale*aspect/2)
110 ymax=ycentre-(scale*aspect/2)
120 ywidth=ymax-ymin
130 max%=8192
140 checksum%=0
150 FOR X%=0 TO xsize%-1 STEP 1
160 FOR Y%=0 TO ysize%-1 STEP 1
170 a=(xwidth*X%/xsize%)+xmin
180 b=(ywidth*Y%/ysize%)+ymin
190 PROCit(a,b,max%)
200 checksum%+=IT%
210 NEXT Y%
220 NEXT X%
230 PRINT checksum%
240 END
250 DEFPROCit(a,b,ITER%)
260 IT%=0
270 e=0
280 f=0
290 REPEAT
300 u=(e*e)-(f*f)
310 v=2*e*f
320 e=u+a
330 f=v+b
340 IT%=IT%+1
350 UNTIL IT%=ITER% OR (ABS(e)+ABS(f))>4
360 ENDPROC
