fn greet() -> String {
    "Hello, world!".to_string()
}

fn main() {
    println!("{}", greet());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_greets() {
        assert_eq!(greet(), "Hello, world!");
    }
}
