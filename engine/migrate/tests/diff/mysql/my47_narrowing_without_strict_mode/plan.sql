alter table readings
  modify column level tinyint,
  modify column code varchar(3),
  modify column kind enum('low','mid'),
  modify column ratio decimal(4,2),
  modify column note varchar(63);
