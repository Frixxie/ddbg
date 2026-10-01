struct Point {
    x: i32,
    y: i32,
}

fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    sum
}

fn main() {
    let point = Point { x: 3, y: 4 };
    let greeting = String::from("hello");
    let total = add(point.x, point.y); // line 14
    println!("{greeting}: {total}");
}
