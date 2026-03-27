CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra
LDFLAGS ?= -lbladeRF -lm

# libbladeRF installed in /usr/local
CFLAGS  += $(shell pkg-config --cflags libbladeRF 2>/dev/null || echo -I/usr/local/include)
LDFLAGS += $(shell pkg-config --libs libbladeRF 2>/dev/null || echo -L/usr/local/lib64)

TARGET = bladerf_bench
GRID_TARGET = bladerf_grid_bench

all: $(TARGET) $(GRID_TARGET)

$(TARGET): bladerf_bench.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

$(GRID_TARGET): bladerf_grid_bench.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

clean:
	rm -f $(TARGET) $(GRID_TARGET) bladerf_native_bench.csv bladerf_native_grid.csv

run: $(TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(TARGET)

run-grid: $(GRID_TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(GRID_TARGET)

.PHONY: all clean run run-grid
