# CachyOS kernels are built with clang/ThinLTO: pass LLVM=1 whenever the kernel config says so.
KERNELRELEASE ?= $(shell uname -r)
KDIR ?= /usr/lib/modules/$(KERNELRELEASE)/build
LLVMFLAG := $(shell grep -q '^CONFIG_CC_IS_CLANG=y' $(KDIR)/.config 2>/dev/null && echo LLVM=1)

all:
	$(MAKE) -C $(KDIR) M=$(CURDIR) $(LLVMFLAG) modules
clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) $(LLVMFLAG) clean
