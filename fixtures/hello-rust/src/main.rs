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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn fails() {
        assert_eq!(add(2, 2), 5);
    }
}
