plugins {
    java
    application
    checkstyle
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.0")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

application {
    mainClass = "example.Hello"
}

checkstyle {
    toolVersion = "10.18.1"
}

tasks.test {
    useJUnitPlatform()
}
