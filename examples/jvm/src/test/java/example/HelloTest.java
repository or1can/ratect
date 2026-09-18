package example;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class HelloTest {
    @Test
    void greetsTheWorld() {
        assertEquals("Hello, world!", Hello.greet());
    }
}
