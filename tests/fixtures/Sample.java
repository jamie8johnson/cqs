package com.example;

import java.util.ArrayList;
import java.util.List;

/**
 * A simple task manager for demonstration.
 */
public class TaskManager {
    private final List<Task> tasks;

    public TaskManager() {
        this.tasks = new ArrayList<>();
    }

    public void addTask(String name, int priority) {
        tasks.add(new Task(name, priority));
    }

    public Task findByName(String name) {
        return tasks.stream()
            .filter(t -> t.getName().equals(name))
            .findFirst()
            .orElse(null);
    }

    public List<Task> getHighPriority(int threshold) {
        return tasks.stream()
            .filter(t -> t.getPriority() >= threshold)
            .toList();
    }

    public int size() {
        return tasks.size();
    }
}

class Task {
    private final String name;
    private final int priority;

    Task(String name, int priority) {
        this.name = name;
        this.priority = priority;
    }

    public String getName() { return name; }
    public int getPriority() { return priority; }
}
