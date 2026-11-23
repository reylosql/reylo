-- loses data: codes.code changes from varchar(4) to char(4), which drops the trailing spaces of its values
-- loses data: codes.raw changes from binary(2) to binary(4), which pads its values with zero bytes to its length
-- loses data: codes.tag changes from varbinary(4) to binary(4), which pads its values with zero bytes to its length
alter table codes
  modify column code char(4),
  modify column tag binary(4),
  modify column raw binary(4);
