import os, sys, time, multiprocessing as mp
# Light, browsing-like load: each worker does a fixed chunk of work 10x per second and logs
# how long the chunk took (ms), so we can see the performance cost of each power setting.
def worker(path):
    with open(path, "a", buffering=1) as f:
        while True:
            t0 = time.perf_counter(); x = 0
            for i in range(60000): x += i * i
            dt = (time.perf_counter() - t0) * 1000
            f.write(f"{time.time():.3f} {dt:.3f}\n")
            time.sleep(max(0.0, 0.1 - dt / 1000))
if __name__ == "__main__":
    for i in range(2): mp.Process(target=worker, args=(f"{sys.argv[1]}.{i}",), daemon=True).start()
    while True: time.sleep(3600)
