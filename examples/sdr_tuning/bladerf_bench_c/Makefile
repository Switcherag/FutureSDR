CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra
LDFLAGS ?= -lbladeRF -lm

# libbladeRF installed in /usr/local
CFLAGS  += $(shell pkg-config --cflags libbladeRF 2>/dev/null || echo -I/usr/local/include)
LDFLAGS += $(shell pkg-config --libs libbladeRF 2>/dev/null || echo -L/usr/local/lib64)

TARGET = bladerf_bench
GRID_TARGET = bladerf_grid_bench
QTUNE_TARGET = bladerf_quick_tune_bench
RAND_TARGET = bladerf_random_freq_bench

all: $(TARGET) $(GRID_TARGET) $(QTUNE_TARGET) $(RAND_TARGET)

$(TARGET): bladerf_bench.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

$(GRID_TARGET): bladerf_grid_bench.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

$(QTUNE_TARGET): bladerf_quick_tune_bench.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

$(RAND_TARGET): bladerf_random_freq_bench.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

clean:
	rm -f $(TARGET) $(GRID_TARGET) $(QTUNE_TARGET) $(RAND_TARGET) bladerf_native_bench.csv bladerf_native_grid.csv bladerf_quick_tune_bench.csv bladerf_random_freq_bench.csv

run: $(TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(TARGET)

run-grid: $(GRID_TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(GRID_TARGET)

run-quick-tune: $(QTUNE_TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(QTUNE_TARGET)

run-random: $(RAND_TARGET)
	LD_LIBRARY_PATH=/usr/local/lib64:$(LD_LIBRARY_PATH) ./$(RAND_TARGET)

.PHONY: all clean run run-grid run-quick-tune run-random
