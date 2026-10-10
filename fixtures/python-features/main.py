import asyncio
import sys
import threading

from shapes import Rect, scale


def countdown(n):
    while n > 0:
        yield n
        n -= 1


def accumulate(values):
    total = 0
    for i, v in enumerate(values):
        total += v * i
    return total


def worker(name, results):
    value = len(name) * 10
    results[name] = value


def run_threads():
    results = {}
    threads = [
        threading.Thread(target=worker, args=(n, results), name=n)
        for n in ("alpha", "beta")
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return results


async def fetch(delay, label):
    await asyncio.sleep(delay)
    return label.upper()


async def gather():
    return await asyncio.gather(fetch(0.01, "a"), fetch(0.02, "b"))


def risky(divisor):
    return 10 / divisor


def main():
    data = {"nums": [1, 2, 3], "nested": {"k": (4, 5)}, "flag": True}
    rect = scale(Rect(2, 3), 2)
    acc = accumulate(list(range(10)))
    steps = list(countdown(3))
    threads = run_threads()
    labels = asyncio.run(gather())
    try:
        risky(0)
    except ZeroDivisionError as err:
        caught = str(err)
    print(data, rect.area(), acc, steps, threads, labels, caught)  # ddbg:print-results
    if "--crash" in sys.argv:
        risky(0)
    if "--spin" in sys.argv:
        ticks = 0
        while True:
            ticks += 1


if __name__ == "__main__":
    main()
