CC ?= cc
CFLAGS ?= -O2 -g -Wall -Wextra -Werror

.PHONY: all clean
all: mutex_counter

mutex_counter: mutex_counter.c
	$(CC) $(CFLAGS) -pthread -o $@ $<

clean:
	rm -f mutex_counter

