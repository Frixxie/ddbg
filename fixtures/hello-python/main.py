from dataclasses import dataclass


@dataclass
class Point:
    x: int
    y: int


def add(a, b):
    total = a + b
    return total


def main():
    point = Point(3, 4)
    greeting = "hello"
    total = add(point.x, point.y)  # line 18
    print(f"{greeting}: {total}")


if __name__ == "__main__":
    main()
