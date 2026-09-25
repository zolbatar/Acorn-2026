10 REM Reduced Mandelbrot for the hosted mode-2 graphics path
20 MODE 2
30 xsize%=640:ysize%=256
40 aspect=ysize%/xsize%
50 xcentre=-0.75:ycentre=0:scale=3.5
60 xmin=xcentre-(scale/2):xmax=xcentre+(scale/2)
70 xwidth=xmax-xmin
80 ymin=ycentre+(scale*aspect/2):ymax=ycentre-(scale*aspect/2)
90 ywidth=ymax-ymin
100 max%=48
110 FOR X%=0 TO xsize%-1
120 FOR Y%=0 TO ysize%-1
130 a=(xwidth*X%/xsize%)+xmin
140 b=(ywidth*Y%/ysize%)+ymin
150 IT%=0:e=0:f=0
160 REPEAT
170 u=(e*e)-(f*f)
180 v=2*e*f
190 e=u+a:f=v+b
200 IT%=IT%+1
210 UNTIL IT%=max% OR (ABS(e)+ABS(f))>4
220 IF (ABS(e)+ABS(f))>4 THEN GCOL 0,(IT% MOD 7)+1 ELSE GCOL 0,0
230 PLOT 69,X%*2,Y%*4
240 NEXT Y%
250 NEXT X%
260 END
