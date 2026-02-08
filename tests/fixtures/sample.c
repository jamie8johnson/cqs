#include <stdio.h>
#include <stdlib.h>

// Simple linked list node
struct Node {
    int data;
    struct Node* next;
};

// Create a new node
struct Node* create_node(int data) {
    struct Node* node = (struct Node*)malloc(sizeof(struct Node));
    if (node != NULL) {
        node->data = data;
        node->next = NULL;
    }
    return node;
}

// Insert at head
void insert_head(struct Node** head, int data) {
    struct Node* new_node = create_node(data);
    new_node->next = *head;
    *head = new_node;
}

// Find element
int find(struct Node* head, int target) {
    while (head != NULL) {
        if (head->data == target) return 1;
        head = head->next;
    }
    return 0;
}

// Free list
void free_list(struct Node* head) {
    while (head != NULL) {
        struct Node* temp = head;
        head = head->next;
        free(temp);
    }
}
