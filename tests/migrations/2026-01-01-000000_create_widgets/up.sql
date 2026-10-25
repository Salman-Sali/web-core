CREATE TABLE widgets (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    CONSTRAINT widgets_name_key UNIQUE (name)
);
