CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra
LDFLAGS ?= -lbladeRF -lm

# libbladeRF installed in /usr/local
CFLAGS  += $(shell pkg-config --cflags libbladeRF 2>/dev/null || echo -I/usr/local/include)
LDFLAGS += $(shell pkg-config --libs libbladeRF 2>/dev/null || echo -L/usr/local/lib64)

TARGET = retune_timing

all: $(TARGET)

$(TARGET): retune_timing.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

clean:
	rm -f $(TARGET) retune_timing.cf32 retune_timing.meta.json

run: $(TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(TARGET)

.PHONY: all clean run
