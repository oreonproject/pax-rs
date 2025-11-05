# Makefile for test

PREFIX ?= /usr/local
BINDIR = $(PREFIX)/bin
LIBDIR = $(PREFIX)/lib

all: test

test:
	@echo "Building test..."
	# Add your build commands here
	@echo "Build complete"

install: test
	@echo "Installing test..."
	# Add your install commands here
	@echo "Installation complete"

clean:
	@echo "Cleaning..."
	# Add your clean commands here
	@echo "Clean complete"

.PHONY: all test install clean
