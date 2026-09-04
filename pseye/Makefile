obj-m := gspca_main.o gspca_ov534.o
gspca_main-y := gspca.o
gspca_ov534-y := ov534.o

all:
	make -C /lib/modules/$(shell uname -r)/build M=$(CURDIR) modules
