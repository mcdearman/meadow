"""Three nested loops over flat lists of floats: the shape of numerical code,
and of nothing else. `i k j` order, so the innermost loop walks in order.

Lists, not numpy. Anyone multiplying matrices in Python for real calls numpy,
and would then be timing a BLAS written in C and Fortran -- which is a fair
thing to know and not a thing this suite is measuring."""

N = 256


def main():
    a = [0.0] * (N * N)
    b = [0.0] * (N * N)
    c = [0.0] * (N * N)
    for i in range(N):
        for j in range(N):
            a[i * N + j] = float((i + j) % 10)
            b[i * N + j] = float((i * j) % 10)
    for i in range(N):
        for k in range(N):
            aik = a[i * N + k]
            row = k * N
            out = i * N
            for j in range(N):
                c[out + j] += aik * b[row + j]
    print(int(sum(c[i * N + i] for i in range(N))))


if __name__ == "__main__":
    main()
