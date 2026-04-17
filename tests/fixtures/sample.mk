# Sample Makefile for testing
CC = gcc
CFLAGS = -Wall -Werror -O2
LDFLAGS = -lm
SRC = $(wildcard src/*.c)
OBJ = $(SRC:.c=.o)
TARGET = main

# Default target
all: $(TARGET)
	@echo "Build complete"

# Link the final binary
$(TARGET): $(OBJ)
	$(CC) $(LDFLAGS) -o $@ $^

# Compile source files
%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

# Run tests
test: $(TARGET)
	./run_tests.sh
	@echo "Tests passed"

# Clean build artifacts
clean:
	rm -rf $(OBJ) $(TARGET)
	rm -rf build/

# Install to system
install: $(TARGET)
	cp $(TARGET) /usr/local/bin/
	chmod 755 /usr/local/bin/$(TARGET)

# Generate documentation
docs:
	doxygen Doxyfile

# Format source code
fmt:
	clang-format -i $(SRC)

.PHONY: all test clean install docs fmt
