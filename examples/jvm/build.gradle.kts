plugins {
    java
    application
    checkstyle
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation("org.junit.jupiter:junit-jupiter:6.1.3")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

application {
    mainClass = "example.Hello"
}

checkstyle {
    toolVersion = "14.1.0"
}

tasks.test {
    useJUnitPlatform()
}
