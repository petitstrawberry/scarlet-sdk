#ifndef SCARLET_SEMAPHORE_H
#define SCARLET_SEMAPHORE_H
#include <pthread.h>
#include <limits.h>

/* Process-private, unnamed semaphores backed by the native pthread runtime. */
typedef struct {
    pthread_mutex_t __mutex;
    pthread_cond_t __condition;
    unsigned int __value;
    unsigned int __waiters;
} sem_t;
#define SEM_VALUE_MAX INT_MAX

#ifdef __cplusplus
extern "C" {
#endif
int sem_init(sem_t *, int, unsigned int);
int sem_destroy(sem_t *);
int sem_post(sem_t *);
int sem_wait(sem_t *);
int sem_trywait(sem_t *);
int sem_timedwait(sem_t *, const struct timespec *);
int sem_getvalue(sem_t *, int *);
#ifdef __cplusplus
}
#endif
#endif
