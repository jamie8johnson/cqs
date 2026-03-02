package com.example.app

/**
 * A generic stack implementation.
 */
class Stack<T> {
    private val items = mutableListOf<T>()

    fun push(item: T) {
        items.add(item)
    }

    fun pop(): T? = items.removeLastOrNull()

    fun peek(): T? = items.lastOrNull()

    val size: Int get() = items.size
}

/**
 * Configuration for the application.
 */
interface Config {
    fun get(key: String): String?
    fun getOrDefault(key: String, default: String): String
}

enum class LogLevel {
    DEBUG,
    INFO,
    WARN,
    ERROR
}

/**
 * Simple logger with configurable level.
 */
class Logger(private val level: LogLevel = LogLevel.INFO) {
    fun log(msg: String, msgLevel: LogLevel = LogLevel.INFO) {
        if (msgLevel.ordinal >= level.ordinal) {
            println("[${msgLevel.name}] $msg")
        }
    }
}

fun formatDuration(seconds: Long): String {
    val hours = seconds / 3600
    val minutes = (seconds % 3600) / 60
    val secs = seconds % 60
    return "${hours}h ${minutes}m ${secs}s"
}
