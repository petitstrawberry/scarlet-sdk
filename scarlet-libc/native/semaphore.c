#include <errno.h>
#include <semaphore.h>

static int fail(int error) { errno = error; return -1; }

int sem_init(sem_t *sem, int shared, unsigned int value) {
    if (!sem || value > SEM_VALUE_MAX) return fail(EINVAL);
    if (shared) return fail(ENOTSUP);
    int error = pthread_mutex_init(&sem->__mutex, NULL);
    if (error) return fail(error);
    error = pthread_cond_init(&sem->__condition, NULL);
    if (error) { pthread_mutex_destroy(&sem->__mutex); return fail(error); }
    sem->__value = value;
    sem->__waiters = 0;
    return 0;
}

int sem_destroy(sem_t *sem) {
    if (!sem) return fail(EINVAL);
    int error = pthread_mutex_lock(&sem->__mutex);
    if (error) return fail(error);
    int busy = sem->__waiters != 0;
    pthread_mutex_unlock(&sem->__mutex);
    if (busy) return fail(EBUSY);
    error = pthread_cond_destroy(&sem->__condition);
    if (error) return fail(error);
    error = pthread_mutex_destroy(&sem->__mutex);
    return error ? fail(error) : 0;
}

int sem_post(sem_t *sem) {
    if (!sem) return fail(EINVAL);
    int error = pthread_mutex_lock(&sem->__mutex);
    if (error) return fail(error);
    if (sem->__value == SEM_VALUE_MAX) error = EOVERFLOW;
    else {
        ++sem->__value;
        if (sem->__waiters) error = pthread_cond_signal(&sem->__condition);
    }
    pthread_mutex_unlock(&sem->__mutex);
    return error ? fail(error) : 0;
}

static int wait(sem_t *sem, const struct timespec *deadline, int nonblocking) {
    if (!sem) return fail(EINVAL);
    int error = pthread_mutex_lock(&sem->__mutex);
    if (error) return fail(error);
    while (!sem->__value) {
        if (nonblocking) { error = EAGAIN; break; }
        if (deadline && (deadline->tv_nsec < 0 || deadline->tv_nsec >= 1000000000L)) {
            error = EINVAL;
            break;
        }
        ++sem->__waiters;
        error = deadline ? pthread_cond_timedwait(&sem->__condition, &sem->__mutex, deadline)
                         : pthread_cond_wait(&sem->__condition, &sem->__mutex);
        --sem->__waiters;
        if (error) break;
    }
    if (!error) --sem->__value;
    pthread_mutex_unlock(&sem->__mutex);
    return error ? fail(error) : 0;
}

int sem_wait(sem_t *sem) { return wait(sem, NULL, 0); }
int sem_trywait(sem_t *sem) { return wait(sem, NULL, 1); }
int sem_timedwait(sem_t *sem, const struct timespec *deadline) {
    if (!deadline) return fail(EINVAL);
    return wait(sem, deadline, 0);
}
int sem_getvalue(sem_t *sem, int *value) {
    if (!sem || !value) return fail(EINVAL);
    int error = pthread_mutex_lock(&sem->__mutex);
    if (error) return fail(error);
    *value = (int)sem->__value;
    pthread_mutex_unlock(&sem->__mutex);
    return 0;
}
